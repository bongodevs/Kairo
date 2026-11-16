use std::sync::{Arc, OnceLock, Weak};

use player::source::{OpenedSource, SourceError, SourceMetadata, SourceReference, Sources};
use player::track::UNKNOWN_AUTHOR;

use super::health::is_busy;
use super::placeholder::PlaceholderLog;
use crate::config::{FallbackPlaceholder, FallbackProvider};

tokio::task_local! {
    static MIRROR_DEPTH: u8;
}

/// One plugin's request to play through another source.
pub(super) struct MirrorSpec {
    pub(super) isrc: Option<String>,
    pub(super) title: String,
    pub(super) author: String,
}

/// The ordered fallback providers used to resolve plugin mirror requests.
pub(crate) struct MirrorChain {
    providers: Vec<FallbackProvider>,
    sources: OnceLock<Weak<Sources>>,
    placeholders: Arc<PlaceholderLog>,
}

impl MirrorChain {
    pub(super) fn new(providers: Vec<FallbackProvider>) -> Self {
        Self {
            providers,
            sources: OnceLock::new(),
            placeholders: Arc::new(PlaceholderLog::default()),
        }
    }

    /// The track-level markers left by generated silence.
    pub(crate) fn placeholders(&self) -> Arc<PlaceholderLog> {
        Arc::clone(&self.placeholders)
    }

    /// Attach the final registry. A weak reference keeps the plugin source -> chain -> registry
    /// loop from owning itself.
    pub(crate) fn install(&self, sources: &Arc<Sources>) {
        let _ = self.sources.set(Arc::downgrade(sources));
    }

    pub(super) async fn open(
        &self,
        source_name: &str,
        reference: SourceReference,
        metadata: SourceMetadata,
        mirror: &MirrorSpec,
    ) -> Result<OpenedSource, SourceError> {
        let Some(sources) = self.sources.get().and_then(Weak::upgrade) else {
            return Err(SourceError::MirrorNotFound {
                source_name: source_name.to_owned(),
                identifier: mirror.title.clone(),
            });
        };

        let mut unknown = None;
        for provider in &self.providers {
            if provider.source == source_name {
                continue;
            }
            let Some(query) = render(provider, mirror) else {
                continue;
            };
            let opened = MIRROR_DEPTH
                .scope(1, sources.open_in_source(&provider.source, SourceReference::new(query)))
                .await;
            let opened = match opened {
                Ok(Some(opened)) => opened,
                Ok(None) => continue,
                Err(SourceError::UnknownSource { name }) => {
                    unknown.get_or_insert(name);
                    continue;
                }
                Err(error) if is_busy(&error) => continue,
                Err(error) => return Err(error),
            };
            return Ok(opened
                .with_source_name(source_name.to_owned())
                .with_reference(reference)
                .with_metadata(metadata));
        }

        if let Some(source_name) = unknown {
            return Err(SourceError::UnknownMirrorTarget { source_name });
        }
        Err(SourceError::MirrorNotFound {
            source_name: source_name.to_owned(),
            identifier: mirror.title.clone(),
        })
    }
}

/// Whether the current task is already opening a fallback track.
pub(super) fn depth() -> u8 {
    MIRROR_DEPTH.try_with(|depth| *depth).unwrap_or(0)
}

fn render(provider: &FallbackProvider, mirror: &MirrorSpec) -> Option<String> {
    let value = match provider.placeholder {
        FallbackPlaceholder::Isrc => mirror
            .isrc
            .as_deref()
            .map(|isrc| isrc.replace('-', ""))
            .filter(|isrc| !isrc.is_empty())?,
        FallbackPlaceholder::Query => {
            let title = mirror.title.trim();
            let author = mirror.author.trim();
            if title.is_empty() {
                return None;
            }
            if author.is_empty() || author == UNKNOWN_AUTHOR {
                title.to_owned()
            } else {
                format!("{title} {author}")
            }
        }
    };
    let placeholder = match provider.placeholder {
        FallbackPlaceholder::Isrc => "%ISRC%",
        FallbackPlaceholder::Query => "%QUERY%",
    };
    Some(provider.template.replace(placeholder, &value))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use player::ExceptionSeverity;
    use player::decode::Millis;
    use player::source::{LoadFuture, OpenFuture, Source, SourceHints, SourceResult, TrackSource};
    use player::track::{Track, TrackInfo};

    use super::*;
    use crate::plugins::health::BUSY_MESSAGE;

    struct InnerSource;

    impl Source for InnerSource {
        fn name(&self) -> &str {
            "inner"
        }

        fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
            let identifier = reference.identifier().to_owned();
            Box::pin(async move {
                let Some(title) = identifier.strip_prefix("inner:") else {
                    return Ok(SourceResult::NotMine);
                };
                let info = TrackInfo {
                    identifier: identifier.clone(),
                    is_seekable: true,
                    author: String::from("Artist"),
                    length: Some(Millis::new(1_000)),
                    is_stream: false,
                    title: title.to_owned(),
                    uri: None,
                    source_name: String::from("inner"),
                    artwork_url: None,
                    isrc: None,
                };
                Ok(SourceResult::Track(Track::with_info(SourceReference::new(identifier), info)))
            })
        }
    }

    impl TrackSource for InnerSource {
        fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
            let reference = track.reference().clone();
            Box::pin(async move {
                Ok(Some(OpenedSource::read_only(
                    "inner",
                    reference,
                    SourceHints::default(),
                    Some(0),
                    Cursor::new(Vec::new()),
                )))
            })
        }
    }

    struct BusySource;

    impl Source for BusySource {
        fn name(&self) -> &str {
            "busy"
        }

        fn load<'a>(&'a self, _: &'a SourceReference) -> LoadFuture<'a> {
            Box::pin(async {
                Err(SourceError::load_failed(BUSY_MESSAGE, ExceptionSeverity::Common))
            })
        }
    }

    impl TrackSource for BusySource {
        fn open<'a>(&'a self, _: &'a Track) -> OpenFuture<'a> {
            Box::pin(async {
                Err(SourceError::load_failed(BUSY_MESSAGE, ExceptionSeverity::Common))
            })
        }
    }

    fn provider(
        source: &str,
        template: &str,
        placeholder: FallbackPlaceholder,
    ) -> FallbackProvider {
        FallbackProvider { source: source.to_owned(), template: template.to_owned(), placeholder }
    }

    #[tokio::test]
    async fn a_query_provider_opens_the_first_result_and_keeps_outer_identity() {
        let mut sources = Sources::new();
        sources.register(InnerSource).expect("inner source");
        let sources = Arc::new(sources);
        let chain =
            MirrorChain::new(vec![provider("inner", "inner:%QUERY%", FallbackPlaceholder::Query)]);
        chain.install(&sources);

        let metadata = SourceMetadata {
            title: Some("Outer".into()),
            author: Some("Singer".into()),
            ..SourceMetadata::default()
        };
        let opened = chain
            .open(
                "outer",
                SourceReference::new("outer:1"),
                metadata,
                &MirrorSpec { isrc: None, title: "Outer".into(), author: "Singer".into() },
            )
            .await
            .expect("mirror");

        assert_eq!(opened.source_name(), "outer");
        assert_eq!(opened.reference().identifier(), "outer:1");
        assert_eq!(opened.metadata().title.as_deref(), Some("Outer"));
    }

    #[tokio::test]
    async fn a_busy_provider_is_skipped_and_the_next_one_opens() {
        let mut sources = Sources::new();
        sources.register(BusySource).expect("busy source");
        sources.register(InnerSource).expect("inner source");
        let sources = Arc::new(sources);
        let chain = MirrorChain::new(vec![
            provider("busy", "busy:%QUERY%", FallbackPlaceholder::Query),
            provider("inner", "inner:%QUERY%", FallbackPlaceholder::Query),
        ]);
        chain.install(&sources);

        let opened = chain
            .open(
                "outer",
                SourceReference::new("outer:1"),
                SourceMetadata::default(),
                &MirrorSpec { isrc: None, title: "Outer".into(), author: "Singer".into() },
            )
            .await
            .expect("second provider opens");

        assert_eq!(opened.source_name(), "outer");
        assert_eq!(opened.reference().identifier(), "outer:1");
    }

    #[tokio::test]
    async fn isrc_providers_are_skipped_without_a_code_and_own_source_is_skipped() {
        let mut sources = Sources::new();
        sources.register(InnerSource).expect("inner source");
        let sources = Arc::new(sources);
        let chain = MirrorChain::new(vec![
            provider("outer", "%QUERY%", FallbackPlaceholder::Query),
            provider("inner", "%ISRC%", FallbackPlaceholder::Isrc),
        ]);
        chain.install(&sources);

        let error = chain
            .open(
                "outer",
                SourceReference::new("outer:1"),
                SourceMetadata::default(),
                &MirrorSpec { isrc: None, title: "Outer".into(), author: UNKNOWN_AUTHOR.into() },
            )
            .await
            .expect_err("no mirror");
        assert!(matches!(error, SourceError::MirrorNotFound { .. }));
    }

    #[tokio::test]
    async fn an_isrc_provider_removes_dashes_before_searching() {
        assert_eq!(
            render(
                &provider("inner", "inner:%ISRC%", FallbackPlaceholder::Isrc),
                &MirrorSpec {
                    isrc: Some("IN-Z03-22-01234".into()),
                    title: "Outer".into(),
                    author: "Singer".into(),
                }
            )
            .as_deref(),
            Some("inner:INZ032201234")
        );
    }
}
