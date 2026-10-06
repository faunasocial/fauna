/// Unified error types for the fauna-core crate.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("encoding: {0}")]
    Encoding(String),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("storage: {0}")]
    Storage(String),
    #[error("network: {0}")]
    Network(String),
}

pub type Result<T> = std::result::Result<T, Error>;
