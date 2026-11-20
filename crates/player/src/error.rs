use thiserror::Error;

/// Failure at a player command or worker boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlayerError {
    #[error("no source matched the track identifier")]
    NoMatches,
    #[error("player task has stopped")]
    Stopped,
    #[error("player worker task panicked or was cancelled")]
    WorkerGone,
    #[error("one decoder block produced more than 16 Opus frames")]
    WorkerBurst,
    #[error(transparent)]
    Source(#[from] crate::source::SourceError),
    #[error(transparent)]
    Decode(#[from] crate::decode::DecodeError),
    #[error(transparent)]
    Pcm(#[from] crate::pcm::PcmError),
    #[error(transparent)]
    Opus(#[from] crate::opus::OpusError),
    #[error(transparent)]
    Buffer(#[from] crate::buffer::BufferError),
}

impl PlayerError {
    pub(crate) fn severity(&self) -> crate::ExceptionSeverity {
        match self {
            Self::NoMatches => crate::ExceptionSeverity::Common,
            Self::Source(error) => error.severity(),
            Self::Decode(error) => error.severity(),
            Self::Stopped
            | Self::WorkerGone
            | Self::WorkerBurst
            | Self::Pcm(_)
            | Self::Opus(_)
            | Self::Buffer(_) => crate::ExceptionSeverity::Fault,
        }
    }
}
