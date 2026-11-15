use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use kairo_core::track::codec::{SourcePayloadCodec, SourceTrackCodec, TrackCodecs};

use crate::source::engine::SourceEngine;
use crate::source::error::{ProviderError, SourceError};
use crate::source::net::LocalAddressProvider;
use crate::source::probe::SourceProbe;
use crate::source::proxy::ProxyConfig;
use crate::source::{HttpSource, LocalSource, Source, Sources, TrackSource};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub http: SourceOptions,
    #[serde(default)]
    pub local: SourceOptions,
}

/// Per-source options shared by the built-in sources.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceOptions {
    /// Overrides the legacy `lavalink.server.sources.<name>` flag when present.
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceConfig {
    pub http: bool,
    pub local: bool,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self { http: true, local: false }
    }
}

pub(crate) type SourcePair = (Arc<dyn Source>, Option<Arc<dyn TrackSource>>);

struct CatalogEntry {
    source: Arc<dyn Source>,
    track: Option<Arc<dyn TrackSource>>,
}

#[derive(Default)]
pub struct SourceCatalog {
    providers: Vec<CatalogEntry>,
}

impl SourceCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a loading-only provider. Playback for its tracks stays unavailable.
    pub fn add<S>(&mut self, source: S)
    where
        S: Source + 'static,
    {
        self.providers.push(CatalogEntry { source: Arc::new(source), track: None });
    }

    /// Add one value that owns both loading and playback.
    pub fn add_combined<S>(&mut self, source: S)
    where
        S: Source + TrackSource + 'static,
    {
        let source = Arc::new(source);
        let loading: Arc<dyn Source> = source.clone();
        let playback: Arc<dyn TrackSource> = source;
        self.providers.push(CatalogEntry { source: loading, track: Some(playback) });
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.providers.iter().map(|entry| entry.source.name())
    }

    pub(crate) fn into_entries(self) -> Vec<SourcePair> {
        self.providers.into_iter().map(|entry| (entry.source, entry.track)).collect()
    }
}

/// Native providers, the source codecs and the optional decoder probe.
pub struct ProviderManager {
    sources: SourceCatalog,
    codecs: TrackCodecs,
    probe: Option<Arc<dyn SourceProbe>>,
    http_timeouts: Option<(Duration, Duration)>,
    http_proxy: Option<ProxyConfig>,
    local_address: Option<LocalAddressProvider>,
}

impl ProviderManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sources: SourceCatalog::new(),
            codecs: TrackCodecs::new(),
            probe: None,
            http_timeouts: None,
            http_proxy: None,
            local_address: None,
        }
    }

    pub fn with_probe(mut self, probe: Arc<dyn SourceProbe>) -> Self {
        self.probe = Some(probe);
        self
    }

    /// Connect and read timeouts for the generic HTTP source. Zero leaves that side unbounded.
    pub fn with_http_timeouts(mut self, connect: Duration, read: Duration) -> Self {
        self.http_timeouts = Some((connect, read));
        self
    }

    /// Outbound proxy for the generic HTTP source, matching `lavalink.server.httpConfig`.
    pub fn with_http_proxy(mut self, proxy: Option<ProxyConfig>) -> Self {
        self.http_proxy = proxy;
        self
    }

    /// Bind generic HTTP requests to the address the provider reports, such as an active route
    /// planner's current address.
    pub fn with_local_address(mut self, provider: Option<LocalAddressProvider>) -> Self {
        self.local_address = provider;
        self
    }

    pub fn register(
        &mut self,
        source: impl Source + TrackSource + 'static,
        codec: impl SourceTrackCodec + 'static,
    ) -> Result<(), ProviderError> {
        let source_name = Source::name(&source);
        let codec_name = codec.source_name();
        if source_name != codec_name {
            return Err(ProviderError::NameMismatch {
                source_name: source_name.to_owned(),
                codec_name: codec_name.to_owned(),
            });
        }
        self.codecs.register(codec)?;
        self.sources.add_combined(source);
        Ok(())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.sources.names()
    }

    pub fn http_source(&self) -> Result<HttpSource, SourceError> {
        match self.http_timeouts {
            Some((connect, read)) => HttpSource::with_options(
                connect,
                read,
                self.http_proxy.as_ref(),
                self.local_address.clone(),
            ),
            None => HttpSource::with_proxy(self.http_proxy.as_ref(), self.local_address.clone()),
        }
    }

    pub fn set_fallback_codec(&mut self, codec: impl SourcePayloadCodec + 'static) {
        self.codecs.set_fallback(codec);
    }

    pub fn build(self, config: SourceConfig) -> Result<Sources, SourceError> {
        let engine = configure(
            config,
            self.sources,
            self.http_timeouts,
            self.http_proxy,
            self.local_address,
        )?;
        let sources = Sources::from_engine(engine).with_codecs(self.codecs);
        Ok(match self.probe {
            Some(probe) => sources.with_probe(probe),
            None => sources,
        })
    }
}

impl Default for ProviderManager {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn configure(
    config: SourceConfig,
    catalog: SourceCatalog,
    http_timeouts: Option<(Duration, Duration)>,
    http_proxy: Option<ProxyConfig>,
    local_address: Option<LocalAddressProvider>,
) -> Result<SourceEngine, SourceError> {
    let mut sources = SourceEngine::new();
    if config.local {
        sources.register(LocalSource)?;
    }
    for (source, track) in catalog.into_entries() {
        sources.register_dyn(source, track)?;
    }
    if config.http {
        let http = match http_timeouts {
            Some((connect, read)) => {
                HttpSource::with_options(connect, read, http_proxy.as_ref(), local_address)?
            }
            None => HttpSource::with_proxy(http_proxy.as_ref(), local_address)?,
        };
        sources.register(http)?;
    }
    Ok(sources)
}

#[cfg(test)]
#[path = "../../tests/source_catalog.rs"]
mod tests;
