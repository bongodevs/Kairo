use crate::gateway::GatewayError;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("gateway: {0}")]
    Gateway(#[from] GatewayError),
    #[error("no supported transport mode offered")]
    NoMode,
    #[error("discovery: {0}")]
    Discovery(String),
    #[error("bad READY address {0:?}")]
    Endpoint(String),
    #[error("session description: {0}")]
    SessionDescription(String),
    #[error("server negotiated mode {server:?} after we selected {wanted:?}")]
    ModeMismatch { server: String, wanted: String },
    #[error("no OS randomness for the connection id")]
    Entropy,
    #[error("this connection has already run to completion")]
    AlreadyRun,
    #[error("the voice connection has stopped")]
    Stopped,
}
