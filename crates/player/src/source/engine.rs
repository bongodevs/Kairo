use std::sync::Arc;
use std::time::Instant;

use tokio::runtime::Handle;
use tracing::Instrument;

use kairo_core::Millis;
use kairo_core::track::codec::{self, DecodeTrack, TrackCodecError, TrackCodecs};
use kairo_core::track::{Playlist, PlaylistKind, SourceData, Track, TrackInfo, TrackSnapshot};
use kairo_core::{OpenedSource, SourceReference, StreamInfo};

use crate::source::error::{LoadError, SourceError};
use crate::source::framework::{Metrics, MetricsSnapshot};
use crate::source::manager::{SourceCatalog, SourceConfig};
use crate::source::probe::SourceProbe;
pub use crate::source::track::{OpenFuture, TrackSource};
pub use crate::source::{LoadFuture, Source, SourceResult};

const MAX_RESOLUTION_STEPS: usize = 5;

#[derive(Debug)]
#[non_exhaustive]
pub enum LoadOutcome {
    Track(Track),
    Playlist(Playlist),
    Search(Vec<Track>),
    Empty,
    Error(LoadError),
}

enum ResolvedSource {
    Empty,
    Track(Track),
    Opened(OpenedSource),
    Playlist(Playlist),
}

#[derive(Clone)]
struct Registration {
    source: Arc<dyn Source>,
    track: Option<Arc<dyn TrackSource>>,
    metrics: Arc<Metrics>,
}

/// Sources in first-match resolution order, each with an optional playback side.
#[derive(Clone, Default)]
pub(crate) struct SourceEngine {
    entries: Vec<Registration>,
}

impl SourceEngine {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register one value that provides both metadata loading and playback.
    pub(crate) fn register<S>(&mut self, source: S) -> Result<(), SourceError>
    where
        S: Source + TrackSource + 'static,
    {
        let source = Arc::new(source);
        let loading: Arc<dyn Source> = source.clone();
        let playback: Arc<dyn TrackSource> = source;
        self.register_dyn(loading, Some(playback))
    }

    pub(crate) fn register_dyn(
        &mut self,
        source: Arc<dyn Source>,
        track: Option<Arc<dyn TrackSource>>,
    ) -> Result<(), SourceError> {
        let name = source.name();
        if self.entries.iter().any(|entry| entry.source.name() == name) {
            return Err(SourceError::DuplicateSource { name: name.to_owned() });
        }
        self.entries.push(Registration { source, track, metrics: Arc::new(Metrics::default()) });
        Ok(())
    }

    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(|entry| entry.source.name())
    }

    /// Per-source counters for one registered name.
    #[must_use]
    pub fn metrics(&self, name: &str) -> Option<MetricsSnapshot> {
        self.entries
            .iter()
            .find(|entry| entry.source.name() == name)
            .map(|entry| entry.metrics.snapshot())
    }

    async fn load_entry(
        &self,
        entry: &Registration,
        reference: &SourceReference,
    ) -> Result<SourceResult, SourceError> {
        let started = Instant::now();
        let span = tracing::debug_span!("source.load", source = entry.source.name());
        let result = entry.source.load(reference).instrument(span).await;
        entry.metrics.record_load(started.elapsed(), result.is_ok());
        if let Err(error) = &result {
            tracing::debug!(source = entry.source.name(), %error, "source load failed");
        }
        result
    }

    async fn open_entry(
        &self,
        entry: &Registration,
        track: &Track,
    ) -> Result<Option<OpenedSource>, SourceError> {
        let track_source = entry
            .track
            .as_ref()
            .ok_or_else(|| SourceError::UnknownSource { name: entry.source.name().to_owned() })?;
        let started = Instant::now();
        let span = tracing::debug_span!("source.open", source = entry.source.name());
        let result = track_source.open(track).instrument(span).await;
        entry.metrics.record_open(started.elapsed(), result.is_ok());
        if let Err(error) = &result {
            tracing::debug!(source = entry.source.name(), %error, "source open failed");
        }
        result
    }

    /// The first source that owns a search prefix.
    pub(crate) fn search_owner(&self, query: &str) -> Option<&str> {
        self.entries
            .iter()
            .map(|entry| entry.source.as_ref())
            .find(|source| source.owns_search(query))
            .map(Source::name)
    }

    async fn resolve(
        &self,
        reference: SourceReference,
    ) -> Result<Option<ResolvedSource>, SourceError> {
        self.resolve_from(reference, None).await
    }

    async fn resolve_from(
        &self,
        reference: SourceReference,
        only: Option<&str>,
    ) -> Result<Option<ResolvedSource>, SourceError> {
        let mut current = reference;
        let mut only = only;

        for _ in 0..MAX_RESOLUTION_STEPS {
            let mut redirected = None;
            for entry in &self.entries {
                if only.is_some_and(|name| entry.source.name() != name) {
                    continue;
                }
                if !entry.source.claims(current.identifier()) {
                    continue;
                }
                match self.load_entry(entry, &current).await? {
                    SourceResult::NotMine => {}
                    SourceResult::Empty => return Ok(Some(ResolvedSource::Empty)),
                    SourceResult::Track(track) => {
                        return Ok(Some(ResolvedSource::Track(track)));
                    }
                    SourceResult::Opened(opened) => {
                        return Ok(Some(ResolvedSource::Opened(opened)));
                    }
                    SourceResult::Playlist(playlist) => {
                        return Ok(Some(ResolvedSource::Playlist(playlist)));
                    }
                    SourceResult::Redirect(reference) => {
                        redirected = Some(reference);
                        break;
                    }
                }
            }

            let Some(reference) = redirected else {
                return Ok(None);
            };
            current = reference;
            only = None;
        }

        Ok(None)
    }

    /// Open the first matching playable track, restarting source selection after redirects.
    pub async fn open(
        &self,
        reference: SourceReference,
    ) -> Result<Option<OpenedSource>, SourceError> {
        let identifier = reference.identifier().to_owned();
        let resolved = self.resolve(reference).await?;
        self.open_resolved(resolved, identifier).await
    }

    /// Open a resolved track through the source named in its metadata.
    pub async fn open_track(&self, track: &Track) -> Result<Option<OpenedSource>, SourceError> {
        if track.info().is_some() {
            return self.open_known_track(track).await;
        }
        let identifier = track.reference().identifier().to_owned();
        let resolved = self.resolve(track.reference().clone()).await?;
        self.open_resolved(resolved, identifier).await
    }

    /// Open a reference through one named source, falling back to the full list after a redirect.
    pub(crate) async fn open_named(
        &self,
        name: &str,
        reference: SourceReference,
    ) -> Result<Option<OpenedSource>, SourceError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.source.name() == name)
            .ok_or_else(|| SourceError::UnknownSource { name: name.to_owned() })?;
        self.open_from(entry, reference).await
    }

    async fn open_from(
        &self,
        entry: &Registration,
        reference: SourceReference,
    ) -> Result<Option<OpenedSource>, SourceError> {
        match self.load_entry(entry, &reference).await? {
            SourceResult::NotMine | SourceResult::Empty => Ok(None),
            SourceResult::Opened(opened) => Ok(Some(opened)),
            SourceResult::Track(track) => self.open_known_track(&track).await,
            SourceResult::Playlist(playlist) => {
                let Some(track) = playlist.selected_track().or_else(|| playlist.tracks().first())
                else {
                    return Ok(None);
                };
                self.open_known_track(track).await
            }
            SourceResult::Redirect(reference) => self.open(reference).await,
        }
    }

    async fn open_resolved(
        &self,
        resolved: Option<ResolvedSource>,
        identifier: String,
    ) -> Result<Option<OpenedSource>, SourceError> {
        match resolved {
            Some(ResolvedSource::Empty) => Ok(None),
            Some(ResolvedSource::Track(track)) => self.open_known_track(&track).await,
            Some(ResolvedSource::Opened(opened)) => Ok(Some(opened)),
            Some(ResolvedSource::Playlist(_)) => {
                Err(SourceError::UnexpectedPlaylist { identifier })
            }
            None => Ok(None),
        }
    }

    async fn open_known_track(&self, track: &Track) -> Result<Option<OpenedSource>, SourceError> {
        let Some(info) = track.info() else {
            return Err(SourceError::UnresolvedTrack);
        };
        let name = &info.source_name;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.source.name() == name)
            .ok_or_else(|| SourceError::UnknownSource { name: name.clone() })?;
        self.open_entry(entry, track).await
    }
}

/// The ordered providers, their track codecs and the decoder probe.
#[derive(Clone)]
pub struct Sources {
    engine: Arc<SourceEngine>,
    codecs: Arc<TrackCodecs>,
    probe: Option<Arc<dyn SourceProbe>>,
}

impl Sources {
    #[must_use]
    pub fn new() -> Self {
        Self::from_engine(SourceEngine::new())
    }

    pub fn configured(config: SourceConfig, catalog: SourceCatalog) -> Result<Self, SourceError> {
        Ok(Self::from_engine(crate::source::manager::configure(config, catalog, None, None, None)?))
    }

    pub(crate) fn from_engine(engine: SourceEngine) -> Self {
        Self { engine: Arc::new(engine), codecs: Arc::new(TrackCodecs::new()), probe: None }
    }

    #[must_use]
    pub fn with_codecs(mut self, codecs: TrackCodecs) -> Self {
        self.codecs = Arc::new(codecs);
        self
    }

    #[must_use]
    pub fn with_probe(mut self, probe: Arc<dyn SourceProbe>) -> Self {
        self.probe = Some(probe);
        self
    }

    pub fn register<S>(&mut self, source: S) -> Result<(), SourceError>
    where
        S: Source + TrackSource + 'static,
    {
        Arc::make_mut(&mut self.engine).register(source)
    }

    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.engine.names()
    }

    /// Per-source counters and latency for one registered name.
    #[must_use]
    pub fn metrics(&self, name: &str) -> Option<MetricsSnapshot> {
        self.engine.metrics(name)
    }

    #[must_use]
    pub fn codecs(&self) -> &TrackCodecs {
        &self.codecs
    }

    #[must_use]
    pub fn codecs_arc(&self) -> Arc<TrackCodecs> {
        Arc::clone(&self.codecs)
    }

    /// Resolve one identifier into the five client-visible loading outcomes.
    pub async fn load(&self, reference: SourceReference) -> LoadOutcome {
        self.outcome(self.engine.resolve(reference).await).await
    }

    /// Search through the provider that owns the query prefix, for example `jssearch:`.
    pub async fn search(&self, query: &str) -> LoadOutcome {
        let owner = self.engine.search_owner(query);
        let resolved = self.engine.resolve_from(SourceReference::new(query), owner).await;
        self.outcome(resolved).await
    }

    async fn outcome(&self, resolved: Result<Option<ResolvedSource>, SourceError>) -> LoadOutcome {
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => return LoadOutcome::Error(error.into()),
        };
        match resolved {
            Some(ResolvedSource::Empty) => LoadOutcome::Empty,
            Some(ResolvedSource::Track(track)) => LoadOutcome::Track(track),
            Some(ResolvedSource::Opened(source)) => match self.resolve_track(source).await {
                Ok(track) => LoadOutcome::Track(track),
                Err(error) => LoadOutcome::Error(error),
            },
            Some(ResolvedSource::Playlist(playlist)) => match playlist.kind() {
                PlaylistKind::Playlist => LoadOutcome::Playlist(playlist),
                PlaylistKind::Search => LoadOutcome::Search(playlist.into_tracks()),
            },
            None => LoadOutcome::Empty,
        }
    }

    /// Open the first matching playable track, restarting source selection after redirects.
    pub async fn open(
        &self,
        reference: SourceReference,
    ) -> Result<Option<OpenedSource>, SourceError> {
        let opened = self.engine.open(reference).await?;
        Ok(opened.map(|opened| super::refresh::wrap(self, opened)))
    }

    /// Open a resolved track through the source named in its metadata.
    pub async fn open_track(&self, track: &Track) -> Result<Option<OpenedSource>, SourceError> {
        let opened = self.engine.open_track(track).await?;
        Ok(opened.map(|opened| super::refresh::wrap(self, opened)))
    }

    /// Open one reference through one named source. Used by the fallback chain.
    pub async fn open_in_source(
        &self,
        source: &str,
        reference: SourceReference,
    ) -> Result<Option<OpenedSource>, SourceError> {
        self.engine.open_named(source, reference).await
    }

    /// Probe opened bytes into the stream information a track is built from.
    pub async fn decode(&self, source: OpenedSource) -> Result<StreamInfo, LoadError> {
        let probe = self.probe.clone().ok_or(LoadError::ProbeUnavailable)?;
        let runtime = Handle::try_current().map_err(|_| LoadError::RuntimeUnavailable)?;
        runtime
            .spawn_blocking(move || probe.probe(source))
            .await
            .map_err(|_| LoadError::WorkerGone)?
            .map_err(LoadError::Probe)
    }

    pub fn encode(&self, track: &TrackSnapshot) -> Result<String, TrackCodecError> {
        codec::encode(track, &self.codecs)
    }

    pub fn decode_track(&self, encoded: &str) -> Result<DecodeTrack, TrackCodecError> {
        codec::decode(encoded, &self.codecs)
    }

    async fn resolve_track(&self, source: OpenedSource) -> Result<Track, LoadError> {
        let reference = source.reference().clone();
        let stream = self.decode(source).await?;
        let info = TrackInfo::from_stream(&stream);
        let source_data = SourceData::probe(stream.container, None);
        Ok(Track::from_parts(reference, info, source_data, Millis::default()))
    }
}

impl Default for Sources {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../../tests/source_engine.rs"]
mod tests;
