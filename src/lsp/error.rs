use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum LspError {
    #[error("LSP method '{method}' not found: {message}")]
    MethodNotFound { method: String, message: String },
    #[error("invalid LSP params: {0}")]
    InvalidParams(String),
    #[error("LSP internal error {code}: {message}")]
    InternalError { code: i64, message: String },
    #[error("LSP request cancelled")]
    Cancelled,
    #[error("LSP request timeout: {0}")]
    Timeout(String),
    #[error("rust-analyzer process exited")]
    ProcessDied,
    #[error("LSP transport error: {0}")]
    Transport(String),
}

impl LspError {
    pub fn from_lsp_error(method: &str, error: &serde_json::Value) -> Self {
        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603);
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown LSP error")
            .to_string();
        match code {
            -32601 => LspError::MethodNotFound {
                method: method.to_string(),
                message,
            },
            -32602 => LspError::InvalidParams(message),
            -32800 => LspError::Cancelled,
            _ => LspError::InternalError { code, message },
        }
    }

    /// Treat "no symbol/out-of-range/index not ready" errors as a lookup miss
    /// (Ok(null)) rather than propagating. rust-analyzer in practice signals
    /// every flavour of "I have nothing useful here" via generic InternalError
    /// -32603 (e.g. `Invalid offset`, `no rust file`) on top of the standard
    /// MethodNotFound and the request-state sentinel codes (-32801..=-32803).
    ///
    /// Callers that want to distinguish a real internal bug from a lookup miss
    /// should rely on the `tracing` logs — `lsp/connection.rs` logs every
    /// non-zero LSP error at `error!` level before this coercion runs.
    pub fn is_no_result(&self) -> bool {
        matches!(
            self,
            LspError::MethodNotFound { .. } | LspError::InternalError { .. }
        )
    }

    /// rust-analyzer signals "no renamable symbol at this position" via InvalidParams (-32602)
    /// instead of returning null. Treat that as a lookup miss for rename/prepareRename callers.
    pub fn is_no_rename_target(&self) -> bool {
        self.is_no_result() || matches!(self, LspError::InvalidParams(_))
    }
}
