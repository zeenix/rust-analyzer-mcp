use std::fmt;

#[derive(Debug, Clone)]
pub enum LspError {
    MethodNotFound { method: String, message: String },
    InvalidParams(String),
    InternalError { code: i64, message: String },
    Cancelled,
    Timeout(String),
    ProcessDied,
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

    pub fn is_no_result(&self) -> bool {
        matches!(
            self,
            LspError::MethodNotFound { .. } | LspError::InternalError { .. }
        )
    }
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LspError::MethodNotFound { method, message } => {
                write!(f, "LSP method '{}' not found: {}", method, message)
            }
            LspError::InvalidParams(m) => write!(f, "invalid LSP params: {}", m),
            LspError::InternalError { code, message } => {
                write!(f, "LSP internal error {}: {}", code, message)
            }
            LspError::Cancelled => write!(f, "LSP request cancelled"),
            LspError::Timeout(m) => write!(f, "LSP request timeout: {}", m),
            LspError::ProcessDied => write!(f, "rust-analyzer process exited"),
            LspError::Transport(m) => write!(f, "LSP transport error: {}", m),
        }
    }
}

impl std::error::Error for LspError {}
