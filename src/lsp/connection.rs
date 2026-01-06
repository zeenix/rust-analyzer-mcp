use log::{debug, error, info, warn};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::{Mutex, Notify},
};

use crate::protocol::lsp::LSPResponse;

use super::client::PendingRequest;

/// Start handlers with crash detection support.
pub fn start_handlers_with_crash_detection(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diagnostics_notify: Arc<Notify>,
    is_crashed: Arc<Mutex<bool>>,
) {
    let is_crashed_stderr = Arc::clone(&is_crashed);
    tokio::spawn(handle_stderr(stderr, is_crashed_stderr));

    let is_crashed_stdout = Arc::clone(&is_crashed);
    tokio::spawn(handle_stdout(
        stdout,
        pending_requests,
        diagnostics,
        diagnostics_notify,
        is_crashed_stdout,
    ));
}



async fn handle_stderr(stderr: tokio::process::ChildStderr, is_crashed: Arc<Mutex<bool>>) {
    let mut reader = BufReader::new(stderr);
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let bytes_read = match reader.read_line(&mut buffer).await {
            Ok(n) => n,
            Err(e) => {
                error!("Error reading rust-analyzer stderr: {}", e);
                *is_crashed.lock().await = true;
                break;
            }
        };

        if bytes_read == 0 {
            warn!("rust-analyzer stderr closed (process may have crashed)");
            *is_crashed.lock().await = true;
            break;
        }

        let trimmed = buffer.trim();
        if !trimmed.is_empty() {
            // Check for panic or crash indicators.
            if trimmed.contains("panic") || trimmed.contains("SIGSEGV") {
                error!("rust-analyzer crash detected: {}", trimmed);
                *is_crashed.lock().await = true;
            } else {
                debug!("rust-analyzer stderr: {}", trimmed);
            }
        }
    }
}

async fn handle_stdout(
    stdout: tokio::process::ChildStdout,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diagnostics_notify: Arc<Notify>,
    is_crashed: Arc<Mutex<bool>>,
) {
    let mut reader = BufReader::new(stdout);
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let result = reader.read_line(&mut buffer).await;

        let bytes_read = match result {
            Ok(n) => n,
            Err(e) => {
                error!("Error reading from rust-analyzer stdout: {}", e);
                *is_crashed.lock().await = true;
                // Cancel all pending requests.
                cancel_all_pending(&pending).await;
                break;
            }
        };

        if bytes_read == 0 {
            warn!("rust-analyzer stdout closed (process terminated)");
            *is_crashed.lock().await = true;
            cancel_all_pending(&pending).await;
            break;
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
        if reader.read_line(&mut buffer).await.is_err() {
            *is_crashed.lock().await = true;
            cancel_all_pending(&pending).await;
            break;
        }

        // Read the JSON content.
        let mut json_buffer = vec![0u8; length];
        if reader.read_exact(&mut json_buffer).await.is_err() {
            *is_crashed.lock().await = true;
            cancel_all_pending(&pending).await;
            break;
        }

        let response_str = String::from_utf8_lossy(&json_buffer);
        debug!("Received LSP message: {}", response_str);

        handle_lsp_message(&json_buffer, &pending, &diagnostics, &diagnostics_notify).await;
    }
}

/// Cancel all pending requests (called on crash).
async fn cancel_all_pending(pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>) {
    let mut pending_lock = pending.lock().await;
    for (id, req) in pending_lock.drain() {
        warn!("Cancelling request {} ({}) due to crash", id, req.method);
        let _ = req.sender.send(serde_json::json!(null));
    }
}

fn parse_content_length(header: &str) -> Option<usize> {
    header
        .strip_prefix("Content-Length: ")
        .and_then(|s| s.trim().parse().ok())
}

async fn handle_lsp_message(
    json_buffer: &[u8],
    pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diagnostics_notify: &Arc<Notify>,
) {
    let Ok(json_value) = serde_json::from_slice::<Value>(json_buffer) else {
        error!(
            "Failed to parse LSP message: {}",
            String::from_utf8_lossy(json_buffer)
        );
        return;
    };

    // Check if it's a notification (has method but no id).
    if json_value.get("method").is_some() && json_value.get("id").is_none() {
        handle_notification(json_value, diagnostics, diagnostics_notify).await;
        return;
    }

    // Try to handle as response.
    let Ok(response) = serde_json::from_value::<LSPResponse>(json_value) else {
        return;
    };

    let Some(id) = response.id else {
        return;
    };

    let mut pending_lock = pending.lock().await;
    let Some(req) = pending_lock.remove(&id) else {
        return;
    };

    if let Some(error) = response.error {
        error!("LSP error for request {} ({}): {}", id, req.method, error);
        let _ = req.sender.send(serde_json::json!(null));
    } else {
        let result = response.result.unwrap_or(serde_json::json!(null));
        debug!("Response for request {} ({})", id, req.method);
        let _ = req.sender.send(result);
    }
}

async fn handle_notification(
    json_value: Value,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diagnostics_notify: &Arc<Notify>,
) {
    let Some(method) = json_value.get("method").and_then(|m| m.as_str()) else {
        return;
    };

    debug!("Received notification: {}", method);

    match method {
        "textDocument/publishDiagnostics" => {
            handle_publish_diagnostics(&json_value, diagnostics, diagnostics_notify).await;
        }
        "$/progress" => {
            // Log progress updates.
            if let Some(params) = json_value.get("params") {
                if let Some(value) = params.get("value") {
                    if let Some(message) = value.get("message").and_then(|m| m.as_str()) {
                        debug!("Progress: {}", message);
                    }
                }
            }
        }
        "window/logMessage" | "window/showMessage" => {
            if let Some(params) = json_value.get("params") {
                if let Some(message) = params.get("message").and_then(|m| m.as_str()) {
                    info!("rust-analyzer: {}", message);
                }
            }
        }
        _ => {
            debug!("Unhandled notification: {}", method);
        }
    }
}

async fn handle_publish_diagnostics(
    json_value: &Value,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diagnostics_notify: &Arc<Notify>,
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

    {
        let mut diag_lock = diagnostics.lock().await;
        diag_lock.insert(uri.to_string(), diags.clone());
        info!("Stored {} diagnostics for {}", diags.len(), uri);
    }

    // Notify waiters that new diagnostics are available.
    diagnostics_notify.notify_waiters();
}
