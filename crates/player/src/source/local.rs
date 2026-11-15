use std::io;
use std::path::Path;

use tokio::fs::File;

use kairo_core::track::Track;
use kairo_core::{OpenedSource, SourceHints, SourceReference};

use crate::source::error::SourceError;
use crate::source::{LoadFuture, OpenFuture, Source, SourceResult, TrackSource};

pub const LOCAL_SOURCE: &str = "local";

/// A seekable source backed by a readable regular file.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalSource;

impl LocalSource {
    async fn open_file(reference: &SourceReference) -> Result<SourceResult, SourceError> {
        let path = Path::new(reference.identifier());
        let metadata = match tokio::fs::metadata(path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Ok(SourceResult::NotMine),
            Err(error) if no_match(&error) => return Ok(SourceResult::NotMine),
            Err(source) => {
                return Err(SourceError::LocalIo {
                    path: reference.identifier().to_owned(),
                    source,
                });
            }
        };
        let file = match File::open(path).await {
            Ok(file) => file,
            Err(error) if no_match(&error) => return Ok(SourceResult::NotMine),
            Err(source) => {
                return Err(SourceError::LocalIo {
                    path: reference.identifier().to_owned(),
                    source,
                });
            }
        };
        let extension =
            path.extension().and_then(|extension| extension.to_str()).map(str::to_owned);
        Ok(SourceResult::Opened(OpenedSource::seekable(
            LOCAL_SOURCE,
            reference.clone(),
            SourceHints { mime_type: None, file_extension: extension },
            Some(metadata.len()),
            file,
        )))
    }
}

impl Source for LocalSource {
    fn name(&self) -> &str {
        LOCAL_SOURCE
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(Self::open_file(reference))
    }
}

impl TrackSource for LocalSource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        Box::pin(
            async move { Self::open_file(track.reference()).await?.into_playback(LOCAL_SOURCE) },
        )
    }
}

fn no_match(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied)
}

#[cfg(test)]
#[path = "../../tests/source_local.rs"]
mod tests;
