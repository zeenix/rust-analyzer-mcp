use anyhow::Result;
use log::{debug, error, info, warn};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::{
    io::{
        AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter,
    },
    sync::{oneshot, watch, Mutex},
    task::JoinHandle,
};

use crate::{protocol::lsp::LSPResponse, uri};

/// What rust-analyzer has published about each document, by normalized URI.
pub type Diagnostics = Arc<Mutex<HashMap<String, Vec<Value>>>>;

/// What a request to rust-analyzer comes back with: its result, or what it says went wrong.
pub type Answer = std::result::Result<Value, String>;

/// The requests waiting for an answer, by the id each was sent under.
pub type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Answer>>>>;

/// The cargo checks rust-analyzer has run, as reported by its progress notifications.
///
/// A check is the only thing that produces the diagnostics rustc gives, so knowing whether one
/// has run since a file changed is the difference between reporting on the code as it is and
/// reporting on the code as it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flycheck {
    /// The checks in flight, by the progress token each reports under. There can be several:
    /// rust-analyzer runs one per workspace.
    running: HashSet<String>,
    /// How many checks have begun, counted so that a check started after some point in time can
    /// be told from one that was already running.
    started: u64,
}

impl Flycheck {
    /// Records a check beginning under `token`.
    pub fn begin(&mut self, token: &str) {
        self.running.insert(token.to_string());
        self.started += 1;
    }

    /// Records the check under `token` ending, whether it ran out or was cancelled to make way
    /// for another.
    pub fn end(&mut self, token: &str) {
        self.running.remove(token);
    }

    /// Whether a check begun since `earlier` has run to completion, with none still going.
    pub fn caught_up_with(&self, earlier: &Self) -> bool {
        self.started > earlier.started && self.running.is_empty()
    }

    /// Whether a check has begun since `earlier`.
    pub fn started_since(&self, earlier: &Self) -> bool {
        self.started > earlier.started
    }

    /// Whether rust-analyzer has yet to report a single check.
    pub fn never_ran_one(&self) -> bool {
        self.started == 0
    }
}

/// The progress token prefix rust-analyzer reports its cargo checks under.
const FLYCHECK_TOKEN: &str = "rust-analyzer/flycheck/";

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

/// What the tasks reading rust-analyzer's output work on: everything they hand a message to,
/// and everything they answer one with.
pub struct Connection<W> {
    /// The requests waiting for an answer, by the id each was sent under.
    pub pending_requests: Pending,
    /// The last diagnostics published for each document.
    pub diagnostics: Diagnostics,
    /// Whether rust-analyzer has any background work in flight.
    pub quiescent: watch::Sender<bool>,
    /// The cargo checks rust-analyzer has run.
    pub flycheck: watch::Sender<Flycheck>,
    /// The writing half, for answering rust-analyzer's own requests.
    pub outgoing: Outgoing<W>,
    /// The settings to answer rust-analyzer's configuration requests with.
    pub settings: Value,
}

/// Spawns the tasks reading rust-analyzer's stdout and stderr, returning the stdout reader's
/// handle: it finishes once rust-analyzer's stdout closes, i.e. once rust-analyzer is gone.
pub fn start_handlers<W: AsyncWrite + Unpin + Send + 'static>(
    stdout: impl AsyncRead + Unpin + Send + 'static,
    stderr: impl AsyncRead + Unpin + Send + 'static,
    connection: Connection<W>,
) -> JoinHandle<()> {
    // Log stderr in background.
    tokio::spawn(handle_stderr(stderr));

    // Start response handler task.
    tokio::spawn(handle_stdout(stdout, connection))
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
    connection: Connection<W>,
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

        handle_lsp_message(&json_buffer, &connection).await;
    }

    // rust-analyzer is gone, so no pending request will ever be answered: fail them now rather
    // than letting each run into the request timeout.
    connection.pending_requests.lock().await.clear();
}

fn parse_content_length(header: &str) -> Option<usize> {
    header
        .strip_prefix("Content-Length: ")
        .and_then(|s| s.trim().parse().ok())
}

async fn handle_lsp_message<W: AsyncWrite + Unpin>(json_buffer: &[u8], connection: &Connection<W>) {
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
            answer_request(&method, id.clone(), &json_value, connection).await;
        }
        (Some(_), None) => handle_notification(json_value, connection).await,
        (None, Some(_)) => handle_response(json_value, &connection.pending_requests).await,
        (None, None) => debug!("Ignoring LSP message that is neither request nor response"),
    }
}

async fn handle_response(json_value: Value, pending: &Pending) {
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
        // What rust-analyzer refused to do and why is the answer, and often the only useful one:
        // "Invalid name `1`: not an identifier" tells whoever asked what to do about it, where a
        // bare nothing leaves them guessing at a rename that quietly did nothing.
        let _ = sender.send(Err(message_of(&error)));
    } else {
        let result = response.result.unwrap_or(json!(null));
        info!("Sending result for request {}: {:?}", id, result);
        let _ = sender.send(Ok(result));
    }
}

/// What an LSP error says, which is its `message` unless it is shaped unexpectedly.
fn message_of(error: &Value) -> String {
    error
        .get("message")
        .and_then(|message| message.as_str())
        .map_or_else(|| error.to_string(), str::to_string)
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
    connection: &Connection<W>,
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
                "result": vec![connection.settings.clone(); sections],
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

    if let Err(e) = send_message(&connection.outgoing, &response).await {
        error!("Failed to answer rust-analyzer's {} request: {}", method, e);
    }
}

async fn handle_notification<W: AsyncWrite + Unpin>(json_value: Value, connection: &Connection<W>) {
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

            let mut diag_lock = connection.diagnostics.lock().await;
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
            connection.quiescent.send_replace(is_quiescent);
        }
        // Progress on whatever rust-analyzer has running. The only ones worth following are the
        // cargo checks, whose token names the workspace being checked.
        "$/progress" => {
            let Some(token) = params.get("token").and_then(|t| t.as_str()) else {
                return;
            };
            if !token.starts_with(FLYCHECK_TOKEN) {
                return;
            }

            match params.pointer("/value/kind").and_then(|k| k.as_str()) {
                Some("begin") => {
                    info!("cargo check started: {}", token);
                    connection
                        .flycheck
                        .send_modify(|flycheck| flycheck.begin(token));
                }
                // Sent when a check finishes and when one is cancelled, which is what a restart
                // does to the check it replaces.
                Some("end") => {
                    info!("cargo check finished: {}", token);
                    connection
                        .flycheck
                        .send_modify(|flycheck| flycheck.end(token));
                }
                _ => {}
            }
        }
        _ => {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::DuplexStream;

    #[tokio::test]
    async fn server_status_tracks_quiescence() {
        let (connection, _rust_analyzer) = connection();
        let status = connection.quiescent.subscribe();

        notify(
            "experimental/serverStatus",
            json!({ "health": "ok", "quiescent": true }),
            &connection,
        )
        .await;
        assert!(*status.borrow());

        notify(
            "experimental/serverStatus",
            json!({ "health": "warning", "quiescent": false, "message": "Loading" }),
            &connection,
        )
        .await;
        assert!(!*status.borrow());
    }

    #[tokio::test]
    async fn server_status_without_quiescent_flag_is_ignored() {
        let (connection, _rust_analyzer) = connection();
        connection.quiescent.send_replace(true);
        let status = connection.quiescent.subscribe();

        notify(
            "experimental/serverStatus",
            json!({ "health": "ok" }),
            &connection,
        )
        .await;

        assert!(*status.borrow());
    }

    #[tokio::test]
    async fn publish_diagnostics_are_stored() {
        let (connection, _rust_analyzer) = connection();

        notify(
            "textDocument/publishDiagnostics",
            json!({ "uri": "file:///a.rs", "diagnostics": [{ "message": "boom" }] }),
            &connection,
        )
        .await;

        assert_eq!(connection.diagnostics.lock().await["file:///a.rs"].len(), 1);
    }

    #[tokio::test]
    async fn closed_stdout_fails_pending_requests() {
        let (connection, _rust_analyzer) = connection();
        let (sender, response) = oneshot::channel();
        let pending = Arc::clone(&connection.pending_requests);
        pending.lock().await.insert(1, sender);

        // An already-closed stdout stands in for a rust-analyzer that died mid-request.
        handle_stdout(tokio::io::empty(), connection).await;

        assert!(response.await.is_err());
        assert!(pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn cargo_checks_are_followed_from_start_to_finish() {
        let (connection, _rust_analyzer) = connection();
        let idle = connection.flycheck.borrow().clone();

        progress("rust-analyzer/flycheck/0", "begin", &connection).await;
        assert!(connection.flycheck.borrow().started_since(&idle));
        assert!(!connection.flycheck.borrow().caught_up_with(&idle));

        // rust-analyzer runs a check per workspace, and one of them finishing is not the end of
        // the checking.
        progress("rust-analyzer/flycheck/1", "begin", &connection).await;
        progress("rust-analyzer/flycheck/0", "end", &connection).await;
        assert!(!connection.flycheck.borrow().caught_up_with(&idle));

        progress("rust-analyzer/flycheck/1", "end", &connection).await;
        assert!(connection.flycheck.borrow().caught_up_with(&idle));
    }

    #[tokio::test]
    async fn a_check_cancelled_for_a_restart_is_not_a_check_that_ran() {
        // Asking for a check while one is running cancels it, and a cancelled check ends the
        // same way a finished one does. What tells them apart is that the restart begins another.
        let (connection, _rust_analyzer) = connection();
        progress("rust-analyzer/flycheck/0", "begin", &connection).await;
        let when_we_asked = connection.flycheck.borrow().clone();

        progress("rust-analyzer/flycheck/0", "end", &connection).await;
        assert!(!connection.flycheck.borrow().caught_up_with(&when_we_asked));

        progress("rust-analyzer/flycheck/0", "begin", &connection).await;
        assert!(!connection.flycheck.borrow().caught_up_with(&when_we_asked));

        progress("rust-analyzer/flycheck/0", "end", &connection).await;
        assert!(connection.flycheck.borrow().caught_up_with(&when_we_asked));
    }

    #[tokio::test]
    async fn progress_on_anything_else_is_not_a_cargo_check() {
        let (connection, _rust_analyzer) = connection();

        for token in [
            "rustAnalyzer/cachePriming",
            "rustAnalyzer/Fetching",
            "rustAnalyzer/Indexing",
        ] {
            progress(token, "begin", &connection).await;
            progress(token, "end", &connection).await;
        }

        assert_eq!(*connection.flycheck.borrow(), Flycheck::default());
    }

    #[tokio::test]
    async fn a_request_from_rust_analyzer_is_not_taken_for_an_answer() {
        // rust-analyzer numbers its own requests from zero, in the same space as ours, so one of
        // these landing on a pending id would answer a question nobody asked with nothing.
        let (connection, mut rust_analyzer) = connection();
        let (sender, mut response) = oneshot::channel();
        connection.pending_requests.lock().await.insert(3, sender);

        deliver(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "window/workDoneProgress/create",
                "params": { "token": "rust-analyzer/flycheck/0" }
            }),
            &connection,
        )
        .await;

        assert!(connection.pending_requests.lock().await.contains_key(&3));
        assert!(response.try_recv().is_err(), "nothing may have been sent");
        let answer = framed(&mut rust_analyzer).await;
        assert_eq!(answer["id"], 3);
        assert_eq!(answer["result"], Value::Null);
    }

    #[tokio::test]
    async fn configuration_requests_are_answered_with_the_settings_we_asked_for() {
        // What comes back replaces the configuration rust-analyzer was started with, so a bare
        // "nothing here" would quietly undo the initialization options.
        let (mut connection, mut rust_analyzer) = connection();
        let settings = json!({ "checkOnSave": { "enable": true } });
        connection.settings = settings.clone();

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
            &connection,
        )
        .await;

        let answer = framed(&mut rust_analyzer).await;
        assert_eq!(answer["id"], 0);
        assert_eq!(answer["result"], json!([settings, settings]));
    }

    #[tokio::test]
    async fn requests_we_cannot_serve_are_declined_rather_than_dropped() {
        // Left unanswered they pile up in rust-analyzer's queue of requests it is waiting on,
        // and it is not the client's place to decide when one no longer matters.
        let (connection, mut rust_analyzer) = connection();

        deliver(
            json!({ "jsonrpc": "2.0", "id": 7, "method": "client/registerCapability" }),
            &connection,
        )
        .await;

        let answer = framed(&mut rust_analyzer).await;
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn what_rust_analyzer_refused_to_do_reaches_whoever_asked() {
        // A bare "no" is indistinguishable from a request that did nothing; the reason is the
        // whole of the answer for anything the user got wrong.
        let (connection, _rust_analyzer) = connection();
        let (sender, response) = oneshot::channel();
        connection.pending_requests.lock().await.insert(1, sender);

        deliver(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32602, "message": "Invalid name `1`: not an identifier" }
            }),
            &connection,
        )
        .await;

        assert_eq!(
            response.await.unwrap(),
            Err("Invalid name `1`: not an identifier".to_string())
        );
    }

    #[tokio::test]
    async fn answers_still_reach_whoever_is_waiting() {
        let (connection, _rust_analyzer) = connection();
        let (sender, response) = oneshot::channel();
        connection.pending_requests.lock().await.insert(1, sender);

        deliver(
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "contents": "docs" } }),
            &connection,
        )
        .await;

        assert_eq!(
            response.await.unwrap().unwrap(),
            json!({ "contents": "docs" })
        );
        assert!(connection.pending_requests.lock().await.is_empty());
    }

    /// A connection with nothing going on yet, along with the end rust-analyzer would read from.
    fn connection() -> (Connection<DuplexStream>, DuplexStream) {
        let (ours, theirs) = tokio::io::duplex(4096);
        let connection = Connection {
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
            quiescent: watch::channel(false).0,
            flycheck: watch::channel(Flycheck::default()).0,
            outgoing: Arc::new(Mutex::new(BufWriter::new(ours))),
            settings: json!({}),
        };

        (connection, theirs)
    }

    /// Feed one message through the classifier.
    async fn deliver(message: Value, connection: &Connection<DuplexStream>) {
        handle_lsp_message(message.to_string().as_bytes(), connection).await;
    }

    /// Feed one notification through the classifier.
    async fn notify(method: &str, params: Value, connection: &Connection<DuplexStream>) {
        deliver(
            json!({ "jsonrpc": "2.0", "method": method, "params": params }),
            connection,
        )
        .await;
    }

    /// Feed one `$/progress` notification through the classifier.
    async fn progress(token: &str, kind: &str, connection: &Connection<DuplexStream>) {
        notify(
            "$/progress",
            json!({ "token": token, "value": { "kind": kind } }),
            connection,
        )
        .await;
    }

    /// The next message written to the connection, unwrapped from its header.
    async fn framed(rust_analyzer: &mut DuplexStream) -> Value {
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
}
