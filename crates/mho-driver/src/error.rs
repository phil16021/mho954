use mho_scpi::ScpiError;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error(transparent)]
    Scpi(#[from] ScpiError),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("scope reported error(s): {0}")]
    Device(String),
    #[error("not supported: {0}")]
    Unsupported(String),
}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Scpi(ScpiError::Io(e))
    }
}

pub type Result<T, E = DriverError> = std::result::Result<T, E>;
