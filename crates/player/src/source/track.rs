use std::future::Future;
use std::pin::Pin;

use kairo_core::OpenedSource;
use kairo_core::track::Track;

use crate::source::error::SourceError;

/// Playable bytes plus the seek and length hints the player needs.
pub type Playback = OpenedSource;

/// Open one resolved track. A missing track is `None`, not an error.
pub type OpenFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Playback>, SourceError>> + Send + 'a>>;

/// The playback side of one source. It owns media access and nothing else.
///
/// The source engine pairs it with a [`crate::source::Source`], so it needs no second name.
pub trait TrackSource: Send + Sync {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a>;
}
