use kairo_core::ExceptionSeverity;
use thiserror::Error;

/// Failure while probing, demuxing or decoding one media stream.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DecodeError {
    #[error("the media container is unsupported or malformed")]
    Probe {
        #[source]
        source: symphonia::core::errors::Error,
    },

    #[error("the media container has no playable audio track")]
    NoAudioTrack,

    #[error("the selected audio track has no codec parameters")]
    MissingCodecParameters,

    #[error("the selected audio codec is unsupported")]
    UnsupportedCodec {
        #[source]
        source: symphonia::core::errors::Error,
    },

    #[error("the media container could not provide another packet")]
    Demux {
        #[source]
        source: symphonia::core::errors::Error,
    },

    #[error("an audio packet could not be decoded")]
    Decode {
        #[source]
        source: symphonia::core::errors::Error,
    },

    #[error("the AAC stream could not be decoded: {message}")]
    Aac { message: String },

    #[error("the decoder produced an invalid audio format")]
    InvalidAudioFormat,

    #[error("the media stream could not seek")]
    Seek {
        #[source]
        source: symphonia::core::errors::Error,
    },

    #[error("Opus supports only mono or stereo tracks, got {channels} channels")]
    UnsupportedOpusChannels { channels: usize },

    #[error("Opus decoder initialization failed: {message}")]
    OpusInitialize { message: &'static str },

    #[error("an always-decode request unexpectedly produced passthrough data")]
    UnexpectedPassthrough,
}

impl DecodeError {
    #[must_use]
    pub const fn severity(&self) -> ExceptionSeverity {
        match self {
            Self::NoAudioTrack
            | Self::UnsupportedCodec { .. }
            | Self::UnsupportedOpusChannels { .. } => ExceptionSeverity::Common,
            Self::Probe { .. }
            | Self::MissingCodecParameters
            | Self::Demux { .. }
            | Self::Decode { .. }
            | Self::Aac { .. }
            | Self::Seek { .. } => ExceptionSeverity::Suspicious,
            Self::InvalidAudioFormat
            | Self::OpusInitialize { .. }
            | Self::UnexpectedPassthrough => ExceptionSeverity::Fault,
        }
    }
}
