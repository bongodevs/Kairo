#![forbid(unsafe_code)]

pub mod filters;
pub mod http;
pub mod media;
pub mod track;

mod types;

pub mod ids {
    pub use crate::types::{ChannelId, GuildId, UserId};
}

pub use media::{
    MediaStream, OpenedSource, SourceHints, SourceMetadata, SourceMetadataUpdates, SourceReference,
};
pub use types::{
    AudioFormat, ChannelId, ExceptionSeverity, GuildId, Millis, StreamInfo, TrackMetadata, UserId,
};
