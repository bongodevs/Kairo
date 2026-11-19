use crate::StreamInfo;

pub const UNKNOWN_TITLE: &str = "Unknown title";
pub const UNKNOWN_AUTHOR: &str = "Unknown artist";

/// Everything a client sees about a resolved track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub identifier: String,
    pub is_seekable: bool,
    pub author: String,
    pub length: Option<crate::Millis>,
    pub is_stream: bool,
    pub title: String,
    pub uri: Option<String>,
    pub source_name: String,
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
}

impl TrackInfo {
    #[must_use]
    pub fn from_stream(stream: &StreamInfo) -> Self {
        Self {
            identifier: stream.identifier.clone(),
            is_seekable: stream.seekable,
            author: stream.metadata.artist.clone().unwrap_or_else(|| UNKNOWN_AUTHOR.to_owned()),
            length: stream.duration,
            is_stream: stream.duration.is_none(),
            title: stream.metadata.title.clone().unwrap_or_else(|| UNKNOWN_TITLE.to_owned()),
            uri: Some(stream.identifier.clone()),
            source_name: stream.source_name.clone(),
            artwork_url: stream.metadata.artwork_url.clone(),
            isrc: stream.metadata.isrc.clone(),
        }
    }
}
