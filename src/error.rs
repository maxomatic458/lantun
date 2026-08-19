use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("iroh connect: {0}")]
    Connect(String),

    #[error("iroh endpoint: {0}")]
    Endpoint(String),

    #[error("connection: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),

    #[error("stream read: {0}")]
    Read(#[from] iroh::endpoint::ReadError),

    #[error("stream write: {0}")]
    Write(#[from] iroh::endpoint::WriteError),

    #[error("stream reset: {0}")]
    ReadToEnd(#[from] iroh::endpoint::ReadToEndError),

    #[error("protocol: {0}")]
    Protocol(String),

    #[error("bincode decode: {0}")]
    Decode(#[from] bincode::error::DecodeError),

    #[error("bincode encode: {0}")]
    Encode(#[from] bincode::error::EncodeError),

    #[error("tunnel already running")]
    AlreadyRunning,

    #[error("tunnel not running")]
    NotRunning,

    #[error("invalid secret: expected 32 bytes")]
    InvalidSecret,

    #[error("version mismatch: peer speaks {peer}, we speak {ours}")]
    VersionMismatch { ours: u16, peer: u16 },

    #[error("shutting down")]
    Shutdown,
}
