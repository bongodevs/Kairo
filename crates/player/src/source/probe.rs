use thiserror::Error;

use kairo_core::{OpenedSource, StreamInfo};

/// Failure while probing opened media into stream information.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ProbeError {
    message: String,
}

impl ProbeError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

/// Decodes the head of one opened source. Implementations run on a blocking worker.
pub trait SourceProbe: Send + Sync {
    fn probe(&self, source: OpenedSource) -> Result<StreamInfo, ProbeError>;
}
