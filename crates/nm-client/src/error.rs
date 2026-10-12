#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    Other(String),
    #[error("call timed out")]
    Timeout,
}

pub(crate) fn err(e: impl std::fmt::Display) -> ClientError {
    ClientError::Other(e.to_string())
}
