use std::io;

use thiserror::Error;

use kairo_core::ExceptionSeverity;
use kairo_core::track::codec::TrackCodecError;

use crate::source::probe::ProbeError;

/// Failure while selecting or opening an audio source.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SourceError {
    #[error("a source named {name:?} is already registered")]
    DuplicateSource { name: String },

    #[error("track belongs to unavailable source {name:?}")]
    UnknownSource { name: String },

    #[error("source {name:?} returned metadata instead of playable media")]
    InvalidPlaybackResult { name: String },

    #[error("source returned a track without resolved metadata")]
    UnresolvedTrack,

    #[error("mirror target source {source_name:?} is not registered")]
    UnknownMirrorTarget { source_name: String },

    #[error("no playable mirror for {identifier:?} in source {source_name:?}")]
    MirrorNotFound { source_name: String, identifier: String },

    #[error("failed to build the HTTP source client")]
    HttpClient {
        #[source]
        source: reqwest::Error,
    },

    #[error("failed to request {identifier}")]
    HttpRequest {
        identifier: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("failed to read HTTP source {identifier}")]
    HttpBody {
        identifier: String,
        #[source]
        source: io::Error,
    },

    #[error("URL {identifier} is not playable (HTTP {status})")]
    UnplayableHttp { identifier: String, status: u16 },

    #[error("URL {identifier} returned an invalid redirect location {location:?}")]
    InvalidRedirect { identifier: String, location: String },

    #[error("failed to open local source {path}")]
    LocalIo {
        path: String,
        #[source]
        source: io::Error,
    },

    #[error("identifier {identifier:?} resolved to a playlist instead of a track")]
    UnexpectedPlaylist { identifier: String },

    #[error("network request failed")]
    Network {
        #[source]
        source: reqwest::Error,
    },

    #[error("response is not valid JSON")]
    Json {
        #[source]
        source: serde_json::Error,
    },

    #[error("response exceeds {max} bytes")]
    ResponseTooLarge { max: usize },

    #[error("{message}")]
    LoadFailed { message: String, severity: ExceptionSeverity },
}

impl SourceError {
    #[must_use]
    pub fn load_failed(message: impl Into<String>, severity: ExceptionSeverity) -> Self {
        Self::LoadFailed { message: message.into(), severity }
    }

    /// The provider understood the request but has no result for it.
    #[must_use]
    pub fn not_found(source: &str, identifier: &str) -> Self {
        Self::load_failed(
            format!("{source}: no result for {identifier}"),
            ExceptionSeverity::Common,
        )
    }

    /// The provider cannot be reached right now; retrying later may work.
    #[must_use]
    pub fn unavailable(source: &str, message: impl std::fmt::Display) -> Self {
        Self::load_failed(
            format!("{source} is unavailable: {message}"),
            ExceptionSeverity::Suspicious,
        )
    }

    /// The request is valid but this provider or transport does not support it.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::load_failed(message, ExceptionSeverity::Common)
    }

    /// An API answered with a non-success status.
    #[must_use]
    pub fn api(source: &str, status: u16) -> Self {
        let severity = if status == 429 || status >= 500 {
            ExceptionSeverity::Suspicious
        } else {
            ExceptionSeverity::Common
        };
        Self::load_failed(format!("{source} API returned HTTP {status}"), severity)
    }

    /// A bug in this project, not in the request.
    #[must_use]
    pub fn fault(source: &str, message: impl std::fmt::Display) -> Self {
        Self::load_failed(format!("{source}: {message}"), ExceptionSeverity::Fault)
    }

    #[must_use]
    pub const fn severity(&self) -> ExceptionSeverity {
        match self {
            Self::DuplicateSource { .. }
            | Self::InvalidPlaybackResult { .. }
            | Self::UnresolvedTrack
            | Self::UnknownMirrorTarget { .. }
            | Self::HttpClient { .. } => ExceptionSeverity::Fault,
            Self::UnknownSource { .. } | Self::MirrorNotFound { .. } => ExceptionSeverity::Common,
            Self::HttpRequest { .. }
            | Self::HttpBody { .. }
            | Self::InvalidRedirect { .. }
            | Self::LocalIo { .. }
            | Self::Network { .. }
            | Self::Json { .. }
            | Self::ResponseTooLarge { .. } => ExceptionSeverity::Suspicious,
            Self::UnplayableHttp { .. } | Self::UnexpectedPlaylist { .. } => {
                ExceptionSeverity::Common
            }
            Self::LoadFailed { severity, .. } => *severity,
        }
    }
}

impl From<reqwest::Error> for SourceError {
    fn from(source: reqwest::Error) -> Self {
        Self::Network { source }
    }
}

impl From<serde_json::Error> for SourceError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json { source }
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoadError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Probe(#[from] ProbeError),
    #[error("track loading requires a running Tokio runtime")]
    RuntimeUnavailable,
    #[error("track loading worker stopped unexpectedly")]
    WorkerGone,
    #[error("track loading has no decoder probe installed")]
    ProbeUnavailable,
    #[error("{message}")]
    Message { message: String, severity: ExceptionSeverity },
}

impl LoadError {
    #[must_use]
    pub fn message(message: impl Into<String>, severity: ExceptionSeverity) -> Self {
        Self::Message { message: message.into(), severity }
    }

    #[must_use]
    pub const fn severity(&self) -> ExceptionSeverity {
        match self {
            Self::Source(error) => error.severity(),
            Self::Probe(_) => ExceptionSeverity::Suspicious,
            Self::RuntimeUnavailable | Self::WorkerGone | Self::ProbeUnavailable => {
                ExceptionSeverity::Fault
            }
            Self::Message { severity, .. } => *severity,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderError {
    #[error("source name {source_name:?} does not match codec name {codec_name:?}")]
    NameMismatch { source_name: String, codec_name: String },
    #[error(transparent)]
    Codec(#[from] TrackCodecError),
}
