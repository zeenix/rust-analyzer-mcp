use anyhow::Result;
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{
        AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter,
    },
    sync::{oneshot, watch, Mutex},
    task::JoinHandle,
};

use crate::{protocol::lsp::LSPResponse, uri};

/// The writing half of the connection to rust-analyzer.
///
/// Shared, because rust-analyzer's own requests are answered by the task reading its stdout
/// while whoever drives the client is writing requests of its own.
pub type Outgoing<W> = Arc<Mutex<BufWriter<W>>>;

/// Writes one LSP message, header and all.
pub async fn send_message<W: AsyncWrite + Unpin>(
    outgoing: &Outgoing<W>,
    message: &Value,
) -> Result<()> {
    let content = serde_json::to_string(message)?;
    let framed = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

    let mut writer = outgoing.lock().await;
    writer.write_all(framed.as_bytes()).await?;
    writer.flush().await?;

    Ok(())
}

/// Spawns the tasks reading rust-analyzer's stdout and stderr, returning the stdout reader's
/// handle: it finishes once rust-analyzer's stdout closes, i.e. once rust-analyzer is gone.
pub fn start_handlers<W: AsyncWrite + Unpin + Send + 'static>(
    stdout: impl AsyncRead + Unpin + Send + 'static,
    stderr: impl AsyncRead + Unpin + Send + 'static,
    pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    quiescent: watch::Sender<bool>,
    outgoing: Outgoing<W>,
    settings: Value,
) -> JoinHandle<()> {
    // Log stderr in background.
    tokio::spawn(handle_stderr(stderr));

    // Start response handler task.
    tokio::spawn(handle_stdout(
        stdout,
        pending_requests,
        diagnostics,
        quiescent,
        outgoing,
        settings,
    ))
}

async fn handle_stderr(stderr: impl AsyncRead + Unpin + Send + 'static) {
    let mut reader = BufReader::new(stderr);
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let bytes_read = match reader.read_line(&mut buffer).await {
            Ok(n) => n,
            Err(e) => {
                error!("Error reading rust-analyzer stderr: {}", e);
                break;
            }
        };

        if bytes_read == 0 {
            break; // EOF
        }

        // rust-analyzer is quiet on stderr by default, so what does show up there (its panic
        // messages above all) is worth keeping at the default log level.
        let trimmed = buffer.trim();
        if !trimmed.is_empty() {
            info!("rust-analyzer stderr: {}", trimmed);
        }
    }
}

async fn handle_stdout<W: AsyncWrite + Unpin>(
    stdout: impl AsyncRead + Unpin + Send + 'static,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    quiescent: watch::Sender<bool>,
    outgoing: Outgoing<W>,
    settings: Value,
) {
    let mut reader = BufReader::new(stdout);
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let Ok(bytes_read) = reader.read_line(&mut buffer).await else {
            error!("Error reading from rust-analyzer stdout");
            break;
        };

        if bytes_read == 0 {
            break; // EOF
        }

        if buffer.trim().is_empty() {
            continue;
        }

        if !buffer.starts_with("Content-Length: ") {
            continue;
        }

        let Some(length) = parse_content_length(&buffer) else {
            continue;
        };

        // Read the empty line.
        buffer.clear();
        let _ = reader.read_line(&mut buffer).await;

        // Read the JSON content.
        let mut json_buffer = vec![0u8; length];
        let Ok(_) = reader.read_exact(&mut json_buffer).await else {
            continue;
        };

        let response_str = String::from_utf8_lossy(&json_buffer);
        debug!("Received LSP message: {}", response_str);

        handle_lsp_message(
            &json_buffer,
            &pending,
            &diagnostics,
            &quiescent,
            &outgoing,
            &settings,
        )
        .await;
    }

    // rust-analyzer is gone, so no pending request will ever be answered: fail them now rather
    // than letting each run into the request timeout.
    pending.lock().await.clear();
}

fn parse_content_length(header: &str) -> Option<usize> {
    header
        .strip_prefix("Content-Length: ")
        .and_then(|s| s.trim().parse().ok())
}

async fn handle_lsp_message<W: AsyncWrite + Unpin>(
    json_buffer: &[u8],
    pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    quiescent: &watch::Sender<bool>,
    outgoing: &Outgoing<W>,
    settings: &Value,
) {
    let Ok(json_value) = serde_json::from_slice::<Value>(json_buffer) else {
        error!(
            "Failed to parse LSP message: {}",
            String::from_utf8_lossy(json_buffer)
        );
        return;
    };

    // What a message is turns entirely on whether it names a method and whether it carries an
    // id. Anything that names a method is rust-analyzer talking to us rather than answering us,
    // and taking one of those for an answer means handing whoever is waiting on that id a reply
    // to a question they never asked -- rust-analyzer numbers its own requests from zero, in the
    // same space as ours.
    match (json_value.get("method"), json_value.get("id")) {
        (Some(method), Some(id)) => {
            let method = method.as_str().unwrap_or_default().to_string();
            answer_request(&method, id.clone(), &json_value, outgoing, settings).await;
        }
        (Some(_), None) => handle_notification(json_value, diagnostics, quiescent).await,
        (None, Some(_)) => handle_response(json_value, pending).await,
        (None, None) => debug!("Ignoring LSP message that is neither request nor response"),
    }
}

async fn handle_response(
    json_value: Value,
    pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
) {
    let Ok(response) = serde_json::from_value::<LSPResponse>(json_value) else {
        return;
    };

    let Some(id) = response.id else {
        return;
    };

    let mut pending_lock = pending.lock().await;
    let Some(sender) = pending_lock.remove(&id) else {
        return;
    };

    if let Some(error) = response.error {
        error!("LSP error for request {}: {}", id, error);
        let _ = sender.send(json!(null));
    } else {
        let result = response.result.unwrap_or(json!(null));
        info!("Sending result for request {}: {:?}", id, result);
        let _ = sender.send(result);
    }
}

/// Answers a request rust-analyzer made of us.
///
/// Every one of them has to be answered exactly once, with the id it came with: rust-analyzer
/// keeps them in a queue it expects to empty, and panics outright on an answer to something it
/// never asked.
async fn answer_request<W: AsyncWrite + Unpin>(
    method: &str,
    id: Value,
    request: &Value,
    outgoing: &Outgoing<W>,
    settings: &Value,
) {
    debug!("Received request from rust-analyzer: {}", method);

    let response = match method {
        // Asked after every `didChangeConfiguration`, and what comes back replaces the
        // configuration rust-analyzer started with -- so this answers with all of it, once per
        // section asked about.
        "workspace/configuration" => {
            let sections = request
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map_or(1, Vec::len);
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": vec![settings.clone(); sections],
            })
        }
        // A progress report is about to start under a token of rust-analyzer's choosing. There
        // is nothing to set up on this side; the answer is the whole point.
        "window/workDoneProgress/create" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null,
        }),
        _ => {
            info!(
                "Declining unsupported request from rust-analyzer: {}",
                method
            );
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}"),
                },
            })
        }
    };

    if let Err(e) = send_message(outgoing, &response).await {
        error!("Failed to answer rust-analyzer's {} request: {}", method, e);
    }
}

async fn handle_notification(
    json_value: Value,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    quiescent: &watch::Sender<bool>,
) {
    let Some(method) = json_value.get("method").and_then(|m| m.as_str()) else {
        return;
    };

    debug!("Received notification: {}", method);

    let Some(params) = json_value.get("params") else {
        return;
    };

    match method {
        "textDocument/publishDiagnostics" => {
            let Some(uri) = params.get("uri").and_then(|u| u.as_str()) else {
                return;
            };

            let Some(diags) = params.get("diagnostics").and_then(|d| d.as_array()) else {
                return;
            };

            let mut diag_lock = diagnostics.lock().await;
            // Keyed the same way the lookups spell it, see `uri::normalize()`.
            diag_lock.insert(uri::normalize(uri), diags.clone());
            info!("Stored {} diagnostics for {}", diags.len(), uri);
        }
        // rust-analyzer's status report, opted into through the `serverStatusNotification`
        // client capability. `quiescent` is false while it has background work in flight, such
        // as loading the workspace.
        "experimental/serverStatus" => {
            let Some(is_quiescent) = params.get("quiescent").and_then(|q| q.as_bool()) else {
                return;
            };

            info!("rust-analyzer reports quiescent: {}", is_quiescent);
            // Only a degraded status comes with a message, so it is always worth surfacing.
            if let Some(message) = params.get("message").and_then(|m| m.as_str()) {
                warn!("rust-analyzer status: {}", message);
            }
            quiescent.send_replace(is_quiescent);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn server_status_tracks_quiescence() {
        let (quiescent, status) = watch::channel(false);

        notify(
            "experimental/serverStatus",
            json!({ "health": "ok", "quiescent": true }),
            &quiescent,
        )
        .await;
        assert!(*status.borrow());

        notify(
            "experimental/serverStatus",
            json!({ "health": "warning", "quiescent": false, "message": "Loading" }),
            &quiescent,
        )
        .await;
        assert!(!*status.borrow());
    }

    #[tokio::test]
    async fn server_status_without_quiescent_flag_is_ignored() {
        let (quiescent, status) = watch::channel(true);
        notify(
            "experimental/serverStatus",
            json!({ "health": "ok" }),
            &quiescent,
        )
        .await;
        assert!(*status.borrow());
    }

    #[tokio::test]
    async fn publish_diagnostics_are_stored() {
        let (quiescent, _status) = watch::channel(false);
        let diagnostics = notify(
            "textDocument/publishDiagnostics",
            json!({ "uri": "file:///a.rs", "diagnostics": [{ "message": "boom" }] }),
            &quiescent,
        )
        .await;
        assert_eq!(diagnostics.lock().await["file:///a.rs"].len(), 1);
    }

    #[tokio::test]
    async fn closed_stdout_fails_pending_requests() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (sender, response) = oneshot::channel();
        pending.lock().await.insert(1, sender);
        let (quiescent, _status) = watch::channel(false);

        // An already-closed stdout stands in for a rust-analyzer that died mid-request.
        handle_stdout(
            tokio::io::empty(),
            Arc::clone(&pending),
            Arc::new(Mutex::new(HashMap::new())),
            quiescent,
            Arc::new(Mutex::new(BufWriter::new(tokio::io::sink()))),
            json!({}),
        )
        .await;

        assert!(response.await.is_err());
        assert!(pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn a_request_from_rust_analyzer_is_not_taken_for_an_answer() {
        // rust-analyzer numbers its own requests from zero, in the same space as ours, so one of
        // these landing on a pending id would answer a question nobody asked with nothing.
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (sender, response) = oneshot::channel();
        pending.lock().await.insert(3, sender);
        let (outgoing, mut rust_analyzer) = outgoing();

        deliver(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "window/workDoneProgress/create",
                "params": { "token": "rust-analyzer/flycheck/0" }
            }),
            &pending,
            &outgoing,
            &json!({}),
        )
        .await;

        assert!(pending.lock().await.contains_key(&3));
        let mut response = response;
        assert!(response.try_recv().is_err(), "nothing may have been sent");
        let answer = framed(&mut rust_analyzer).await;
        assert_eq!(answer["id"], 3);
        assert_eq!(answer["result"], Value::Null);
    }

    #[tokio::test]
    async fn configuration_requests_are_answered_with_the_settings_we_asked_for() {
        // What comes back replaces the configuration rust-analyzer was started with, so a bare
        // "no configuration here" would quietly undo the initialization options.
        let settings = json!({ "checkOnSave": { "enable": true } });
        let (outgoing, mut rust_analyzer) = outgoing();

        deliver(
            json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "workspace/configuration",
                "params": { "items": [
                    { "section": "rust-analyzer" },
                    { "section": "rust-analyzer", "scopeUri": "file:///ws" }
                ] }
            }),
            &Arc::new(Mutex::new(HashMap::new())),
            &outgoing,
            &settings,
        )
        .await;

        let answer = framed(&mut rust_analyzer).await;
        assert_eq!(answer["id"], 0);
        assert_eq!(answer["result"], json!([settings, settings]));
    }

    #[tokio::test]
    async fn requests_we_cannot_serve_are_declined_rather_than_dropped() {
        // Left unanswered they pile up in rust-analyzer's queue of requests it is still waiting
        // on, and it is not the client's place to decide when one no longer matters.
        let (outgoing, mut rust_analyzer) = outgoing();

        deliver(
            json!({ "jsonrpc": "2.0", "id": 7, "method": "client/registerCapability" }),
            &Arc::new(Mutex::new(HashMap::new())),
            &outgoing,
            &json!({}),
        )
        .await;

        let answer = framed(&mut rust_analyzer).await;
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn answers_still_reach_whoever_is_waiting() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (sender, response) = oneshot::channel();
        pending.lock().await.insert(1, sender);
        let (outgoing, _rust_analyzer) = outgoing();

        deliver(
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "contents": "docs" } }),
            &pending,
            &outgoing,
            &json!({}),
        )
        .await;

        assert_eq!(response.await.unwrap(), json!({ "contents": "docs" }));
        assert!(pending.lock().await.is_empty());
    }

    /// Feed one message through the classifier.
    async fn deliver(
        message: Value,
        pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
        outgoing: &Outgoing<tokio::io::DuplexStream>,
        settings: &Value,
    ) {
        let (quiescent, _status) = watch::channel(false);
        handle_lsp_message(
            message.to_string().as_bytes(),
            pending,
            &Arc::new(Mutex::new(HashMap::new())),
            &quiescent,
            outgoing,
            settings,
        )
        .await;
    }

    /// An outgoing half of a connection, along with the end rust-analyzer would read from.
    fn outgoing() -> (Outgoing<tokio::io::DuplexStream>, tokio::io::DuplexStream) {
        let (ours, theirs) = tokio::io::duplex(4096);
        (Arc::new(Mutex::new(BufWriter::new(ours))), theirs)
    }

    /// The next message written to the connection, unwrapped from its header.
    async fn framed(rust_analyzer: &mut tokio::io::DuplexStream) -> Value {
        let mut buffer = vec![0u8; 4096];
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rust_analyzer.read(&mut buffer),
        )
        .await
        .expect("a message must be written")
        .unwrap();

        let message = String::from_utf8_lossy(&buffer[..read]).to_string();
        let (header, body) = message.split_once("\r\n\r\n").expect("{message}");
        assert!(header.starts_with("Content-Length: "), "{header}");

        serde_json::from_str(body).unwrap()
    }

    /// Feed one notification through `handle_notification` and return the diagnostics store.
    async fn notify(
        method: &str,
        params: Value,
        quiescent: &watch::Sender<bool>,
    ) -> Arc<Mutex<HashMap<String, Vec<Value>>>> {
        let diagnostics = Arc::new(Mutex::new(HashMap::new()));
        let notification = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        handle_notification(notification, &diagnostics, quiescent).await;
        diagnostics
    }
}
