use log::{debug, error, info};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::{oneshot, Mutex},
};

use crate::protocol::lsp::LSPResponse;

pub fn start_handlers(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
) {
    // Log stderr in background.
    tokio::spawn(handle_stderr(stderr));

    // Start response handler task.
    tokio::spawn(handle_stdout(stdout, pending_requests, diagnostics));
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
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
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

        handle_lsp_message(&json_buffer, &pending, &diagnostics).await;
    }
}

fn parse_content_length(header: &str) -> Option<usize> {
    header
        .strip_prefix("Content-Length: ")
        .and_then(|s| s.trim().parse().ok())
}

async fn handle_lsp_message(
    json_buffer: &[u8],
    pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
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
        handle_notification(json_value, diagnostics).await;
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
    let Some(sender) = pending_lock.remove(&id) else {
        return;
    };

    if let Some(error) = response.error {
        error!("LSP error for request {}: {}", id, error);
        let _ = sender.send(serde_json::json!(null));
    } else {
        let result = response.result.unwrap_or(serde_json::json!(null));
        info!("Sending result for request {}: {:?}", id, result);
        let _ = sender.send(result);
    }
}

async fn handle_notification(
    json_value: Value,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
) {
    let Some(method) = json_value.get("method").and_then(|m| m.as_str()) else {
        return;
    };

    debug!("Received notification: {}", method);

    if method != "textDocument/publishDiagnostics" {
        return;
    }

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
