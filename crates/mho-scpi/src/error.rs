use std::io;

#[derive(Debug, thiserror::Error)]
pub enum ScpiError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("timeout waiting for response to `{0}`")]
    Timeout(String),
    #[error("invalid response to `{cmd}`: {msg}")]
    Parse { cmd: String, msg: String },
    #[error("device not found: {0}")]
    NotFound(String),
    #[error("device error: {0}")]
    Device(String),
}

impl ScpiError {
    pub fn parse(cmd: &str, msg: impl Into<String>) -> Self {
        ScpiError::Parse { cmd: cmd.to_string(), msg: msg.into() }
    }

    pub fn is_timeout(&self) -> bool {
        match self {
            ScpiError::Timeout(_) => true,
            ScpiError::Io(e) => is_timeout_io(e),
            _ => false,
        }
    }
}

pub(crate) fn is_timeout_io(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

pub type Result<T, E = ScpiError> = std::result::Result<T, E>;
