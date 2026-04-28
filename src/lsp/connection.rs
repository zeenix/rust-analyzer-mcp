use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin},
    sync::{oneshot, watch, Mutex},
};
use tracing::{debug, error, info, warn};

use crate::{
    config::PROCESS_MONITOR_INTERVAL_MILLIS, lsp::error::LspError, protocol::lsp::LSPResponse,
};

pub type ResponseSender = oneshot::Sender<Result<Value, LspError>>;
pub type PendingRequests = Arc<parking_lot::Mutex<HashMap<u64, PendingEntry>>>;
/// Per-token watch channels keyed by `$/progress` token. The bool payload is
/// `true` when the work-done sequence has reported `end`. Stored as
/// `watch::Sender` so late subscribers see the latest value (level-triggered).
pub type ProgressMap = Arc<parking_lot::Mutex<HashMap<String, watch::Sender<bool>>>>;
pub type SharedStdin = Arc<Mutex<Option<BufWriter<ChildStdin>>>>;

pub struct PendingEntry {
    pub method: String,
    pub sender: ResponseSender,
}

pub fn start_handlers(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    stdin: SharedStdin,
    pending_requests: PendingRequests,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    progress: ProgressMap,
) {
    // Log stderr in background.
    tokio::spawn(handle_stderr(stderr));

    // Start response handler task.
    tokio::spawn(handle_stdout(
        stdout,
        stdin,
        pending_requests,
        diagnostics,
        progress,
    ));
}

async fn handle_stderr(stderr: tokio::process::ChildStderr) {
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

        let trimmed = buffer.trim();
        if !trimmed.is_empty() {
            debug!("rust-analyzer stderr: {}", trimmed);
        }
    }
}

async fn handle_stdout(
    stdout: tokio::process::ChildStdout,
    stdin: SharedStdin,
    pending: PendingRequests,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    progress: ProgressMap,
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

        handle_lsp_message(&json_buffer, &stdin, &pending, &diagnostics, &progress).await;
    }
}

fn parse_content_length(header: &str) -> Option<usize> {
    header
        .strip_prefix("Content-Length: ")
        .and_then(|s| s.trim().parse().ok())
}

async fn handle_lsp_message(
    json_buffer: &[u8],
    stdin: &SharedStdin,
    pending: &PendingRequests,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    progress: &ProgressMap,
) {
    let Ok(json_value) = serde_json::from_slice::<Value>(json_buffer) else {
        error!(
            "Failed to parse LSP message: {}",
            String::from_utf8_lossy(json_buffer)
        );
        return;
    };

    let has_method = json_value.get("method").is_some();
    let has_id = json_value.get("id").is_some();

    // Notification (method, no id).
    if has_method && !has_id {
        handle_notification(json_value, diagnostics, progress).await;
        return;
    }

    // Server-initiated request (method + id). We must respond.
    if has_method && has_id {
        handle_server_request(json_value, stdin).await;
        return;
    }

    // Otherwise: response to a request we sent.
    let Ok(response) = serde_json::from_value::<LSPResponse>(json_value) else {
        return;
    };

    let Some(id) = response.id else {
        return;
    };

    let entry = pending.lock().remove(&id);
    let Some(PendingEntry { method, sender }) = entry else {
        return;
    };

    if let Some(error) = response.error {
        error!("LSP error for request {} ({}): {}", id, method, error);
        let _ = sender.send(Err(LspError::from_lsp_error(&method, &error)));
    } else {
        let result = response.result.unwrap_or(serde_json::json!(null));
        info!(
            "Sending result for request {} ({}): {:?}",
            id, method, result
        );
        let _ = sender.send(Ok(result));
    }
}

async fn handle_notification(
    json_value: Value,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    progress: &ProgressMap,
) {
    let Some(method) = json_value.get("method").and_then(|m| m.as_str()) else {
        return;
    };

    debug!("Received notification: {}", method);

    match method {
        "textDocument/publishDiagnostics" => {
            handle_publish_diagnostics(&json_value, diagnostics).await;
        }
        "$/progress" => {
            handle_progress(&json_value, progress);
        }
        _ => {}
    }
}

async fn handle_publish_diagnostics(
    json_value: &Value,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
) {
    let Some(params) = json_value.get("params") else {
        return;
    };

    let Some(uri) = params.get("uri").and_then(|u| u.as_str()) else {
        return;
    };

    let Some(diags) = params.get("diagnostics").and_then(|d| d.as_array()) else {
        return;
    };

    let mut diag_lock = diagnostics.lock().await;
    diag_lock.insert(uri.to_string(), diags.clone());
    info!("Stored {} diagnostics for {}", diags.len(), uri);
}

fn handle_progress(json_value: &Value, progress: &ProgressMap) {
    let Some(params) = json_value.get("params") else {
        return;
    };

    // Token may be string or number per LSP spec; rust-analyzer uses strings.
    let token = match params.get("token") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return,
    };

    let Some(kind) = params.pointer("/value/kind").and_then(|k| k.as_str()) else {
        return;
    };

    debug!("Progress {} kind={}", token, kind);

    let mut map = progress.lock();
    let sender = map.entry(token.clone()).or_insert_with(|| {
        let (tx, _) = watch::channel(false);
        tx
    });

    match kind {
        "begin" | "report" => {
            // Don't downgrade an already-completed token.
            if !*sender.borrow() {
                let _ = sender.send(false);
            }
        }
        "end" => {
            let _ = sender.send(true);
            info!("Progress {} ended", token);
        }
        _ => {}
    }
}

/// Watches the rust-analyzer subprocess. When the process exits — for any
/// reason other than an explicit shutdown that already removed it from the
/// mutex — flips `process_died`, clears the `Child` slot, and fails every
/// pending request with `LspError::ProcessDied` so callers don't hang on a
/// dead transport.
pub fn spawn_process_monitor(
    process: Arc<Mutex<Option<Child>>>,
    process_died: Arc<AtomicBool>,
    pending: PendingRequests,
) {
    tokio::spawn(async move {
        let interval = Duration::from_millis(PROCESS_MONITOR_INTERVAL_MILLIS);
        loop {
            tokio::time::sleep(interval).await;

            let mut guard = process.lock().await;
            let Some(child) = guard.as_mut() else {
                // Either never started or shutdown() took it. Either way, our
                // job is done.
                return;
            };

            match child.try_wait() {
                Ok(None) => {} // still running
                Ok(Some(status)) => {
                    warn!("rust-analyzer process exited: {:?}", status);
                    *guard = None;
                    drop(guard);
                    process_died.store(true, Ordering::Release);
                    drain_pending_with_process_died(&pending);
                    return;
                }
                Err(e) => {
                    warn!("Error polling rust-analyzer status: {}", e);
                    *guard = None;
                    drop(guard);
                    process_died.store(true, Ordering::Release);
                    drain_pending_with_process_died(&pending);
                    return;
                }
            }
        }
    });
}

fn drain_pending_with_process_died(pending: &PendingRequests) {
    let drained: Vec<_> = pending.lock().drain().collect();
    for (_id, entry) in drained {
        let _ = entry.sender.send(Err(LspError::ProcessDied));
    }
}

async fn handle_server_request(json_value: Value, stdin: &SharedStdin) {
    let method = json_value
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let id = json_value.get("id").cloned().unwrap_or(Value::Null);

    debug!("Server request: {} (id={})", method, id);

    // Reply with `null` to acknowledge. `window/workDoneProgress/create` is the
    // common case — rust-analyzer may also send `client/registerCapability` etc.
    // We don't actually register anything, but a successful empty reply keeps
    // the server happy.
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": null,
    });

    let content = match serde_json::to_string(&response) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to serialize server-request response: {}", e);
            return;
        }
    };
    let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

    let mut guard = stdin.lock().await;
    let Some(writer) = guard.as_mut() else {
        warn!("Cannot reply to server request {}: no stdin", method);
        return;
    };
    if let Err(e) = writer.write_all(message.as_bytes()).await {
        warn!("Failed to write server-request response: {}", e);
        return;
    }
    if let Err(e) = writer.flush().await {
        warn!("Failed to flush server-request response: {}", e);
    }
}
