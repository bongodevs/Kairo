use std::future::Future;
use std::pin::Pin;

use kairo_core::track::{Playlist, Track};

pub use kairo_core::{MediaStream, OpenedSource, SourceHints, SourceMetadata, SourceReference};

use super::error::SourceError;

/// One metadata lookup. Boxing happens once per lookup, never while reading media bytes.
pub type LoadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SourceResult, SourceError>> + Send + 'a>>;

/// The result of asking one source about an identifier.
#[derive(Debug)]
#[non_exhaustive]
pub enum SourceResult {
    NotMine,
    Empty,
    Track(Track),
    /// Playback bytes from a direct transport. Named providers return `Track` metadata instead
    /// and open playback through a `Stream`.
    Opened(OpenedSource),
    Playlist(Playlist),
    Redirect(SourceReference),
}

impl SourceResult {
    /// Map a direct lookup onto the playback contract.
    pub(crate) fn into_playback(self, name: &str) -> Result<Option<OpenedSource>, SourceError> {
        match self {
            Self::Opened(opened) => Ok(Some(opened)),
            Self::NotMine | Self::Empty => Ok(None),
            _ => Err(SourceError::InvalidPlaybackResult { name: name.to_owned() }),
        }
    }
}

/// The loading side of one source: claims, search prefixes and metadata.
pub trait Source: Send + Sync {
    fn name(&self) -> &str;

    /// A quick identifier check. `false` skips `load`; `true` does not force a match.
    fn claims(&self, _identifier: &str) -> bool {
        true
    }

    /// Prefixes this source owns for `search`. Empty means no prefix.
    fn search_prefixes(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether this source owns a prefixed search query.
    ///
    /// The default checks [`search_prefixes`](Self::search_prefixes); declarative providers
    /// override it so their prefixes need no static storage.
    fn owns_search(&self, query: &str) -> bool {
        self.search_prefixes().iter().any(|prefix| query.starts_with(prefix))
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a>;
}
