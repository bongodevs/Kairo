//! Declarative provider building blocks: prefixes, URL routes, loader/opener closures and metrics.
//!
//! In-tree transports no longer register providers here: every service provider lives in the
//! `crates/sources` cdylib. This surface is retained as the typed extension point for a future
//! in-tree provider, and is currently exercised by the `source_framework` tests only.

mod config;
mod metrics;
mod request;
mod stream;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use kairo_core::track::{Playlist, Track};
use kairo_core::{OpenedSource, SourceMetadata, SourceReference};
use reqwest::redirect::Policy;

use super::{HttpSource, proxy::ProxyConfig};
use crate::source::error::SourceError;
use crate::source::{LoadFuture, OpenFuture, Source, SourceResult, TrackSource};

pub use config::{Settings, read as read_config, read_with};
pub use metrics::{Metrics, MetricsSnapshot};
pub use request::{Kind, Request};
pub use stream::Stream;

use request::{PrefixRoute, PrefixTarget, Route};

/// A boxed loader result.
pub type LoadResultFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Load, SourceError>> + Send + 'a>>;

/// A boxed opener result.
pub type StreamFuture<'a> = Pin<Box<dyn Future<Output = Result<Stream, SourceError>> + Send + 'a>>;

/// What a declarative loader may return.
#[derive(Debug)]
pub enum Load {
    Track(Track),
    Playlist(Playlist),
    Search(Vec<Track>),
    Empty,
    Redirect(SourceReference),
}

/// Turns one parsed request into metadata.
pub trait Loader: Send + Sync {
    fn load<'a>(&'a self, request: Request) -> LoadResultFuture<'a>;
}

impl<F, Fut> Loader for F
where
    F: Fn(Request) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Load, SourceError>> + Send + 'static,
{
    fn load<'a>(&'a self, request: Request) -> LoadResultFuture<'a> {
        Box::pin(self(request))
    }
}

/// Turns one resolved track into a stream description.
pub trait Opener: Send + Sync {
    fn open<'a>(&'a self, track: Track) -> StreamFuture<'a>;
}

impl<F, Fut> Opener for F
where
    F: Fn(Track) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Stream, SourceError>> + Send + 'static,
{
    fn open<'a>(&'a self, track: Track) -> StreamFuture<'a> {
        Box::pin(self(track))
    }
}

/// A provider declared by name, prefixes, URL routes, a loader and an opener.
pub struct Provider {
    name: String,
    enabled: bool,
    prefixes: Vec<PrefixRoute>,
    routes: Vec<Route>,
    loader: Option<Arc<dyn Loader>>,
    opener: Option<Arc<dyn Opener>>,
    proxy: Option<ProxyConfig>,
    http: OnceLock<HttpSource>,
}

impl Provider {
    /// Start a provider. It is enabled until [`enabled`](Self::enabled) says otherwise.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            prefixes: Vec::new(),
            routes: Vec::new(),
            loader: None,
            opener: None,
            proxy: None,
            http: OnceLock::new(),
        }
    }

    /// Declare a search prefix such as `jss:`.
    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        self.prefixes.push(PrefixRoute::new(&prefix, PrefixTarget::Search));
        self
    }

    /// Declare an item prefix such as `jsrec:`.
    #[must_use]
    pub fn item_prefix(mut self, prefix: impl Into<String>, kind: Kind) -> Self {
        let prefix = prefix.into();
        self.prefixes.push(PrefixRoute::new(&prefix, PrefixTarget::Item(kind)));
        self
    }

    /// Declare a URL route. The host is exact; the template may contain one or more `{name}`
    /// segments, and the segment named `{id}` becomes the request identifier.
    #[must_use]
    pub fn route(mut self, host: impl Into<String>, template: &str, kind: Kind) -> Self {
        let host = host.into();
        self.routes.push(Route::new(&host, template, kind));
        self
    }

    /// Provide the metadata loader.
    #[must_use]
    pub fn loader<F, Fut>(mut self, loader: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Load, SourceError>> + Send + 'static,
    {
        self.loader = Some(Arc::new(loader));
        self
    }

    /// Provide the playback opener. Without one the provider only loads metadata.
    #[must_use]
    pub fn opener<F, Fut>(mut self, opener: F) -> Self
    where
        F: Fn(Track) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Stream, SourceError>> + Send + 'static,
    {
        self.opener = Some(Arc::new(opener));
        self
    }

    /// Enable or disable every claim this provider makes.
    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Use this proxy for the streams the framework opens for the provider.
    #[must_use]
    pub fn with_proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Use a prepared client for stream requests instead of the framework default.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http = OnceLock::from(HttpSource::from_client(client));
        self
    }

    /// The registered name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    async fn load_request(&self, reference: &SourceReference) -> Result<SourceResult, SourceError> {
        if !self.enabled {
            return Ok(SourceResult::NotMine);
        }
        let Some(request) = request::parse(&self.prefixes, &self.routes, reference.identifier())
        else {
            return Ok(SourceResult::NotMine);
        };
        let Some(loader) = &self.loader else {
            return Ok(SourceResult::NotMine);
        };
        let query = match &request {
            Request::Search(query) => query.clone(),
            Request::Item(_, _) => String::new(),
        };
        let load = loader.load(request).await?;
        Ok(match load {
            Load::Track(track) => SourceResult::Track(track),
            Load::Playlist(playlist) => SourceResult::Playlist(playlist),
            Load::Search(tracks) if tracks.is_empty() => SourceResult::Empty,
            Load::Search(tracks) => SourceResult::Playlist(Playlist::search(
                format!("{} Search: {query}", self.name),
                tracks,
            )),
            Load::Empty => SourceResult::Empty,
            Load::Redirect(reference) => SourceResult::Redirect(reference),
        })
    }

    async fn open_stream(&self, track: &Track) -> Result<Option<OpenedSource>, SourceError> {
        if !self.enabled {
            return Ok(None);
        }
        let Some(opener) = &self.opener else {
            return Ok(None);
        };
        let stream = opener.open(track.make_clone()).await?;
        let reference = match stream.target {
            stream::Target::Url(url) => SourceReference::new(url),
            stream::Target::Mirror(mirror) => {
                let reference = mirror.reference().clone();
                if !crate::source::net::is_direct(reference.identifier()) {
                    return Err(SourceError::unsupported(format!(
                        "{}: mirror target {} is not a direct stream",
                        self.name,
                        reference.identifier()
                    )));
                }
                reference
            }
        };
        let Some(opened) = self.http()?.open_with_headers(reference, &stream.headers).await? else {
            return Ok(None);
        };
        let metadata = track.info().map_or_else(SourceMetadata::default, SourceMetadata::from);
        let mut hints = opened.hints().clone();
        if let Some(content_type) = stream.content_type {
            hints.mime_type = Some(content_type);
        }
        let content_length = stream.length.or_else(|| opened.content_length());
        let seekable = stream.seekable.unwrap_or_else(|| opened.is_seekable());
        Ok(Some(
            opened
                .with_stream_info(hints, content_length, seekable)
                .with_source_name(self.name.clone())
                .with_reference(track.reference().clone())
                .with_metadata(metadata),
        ))
    }

    fn http(&self) -> Result<&HttpSource, SourceError> {
        if let Some(http) = self.http.get() {
            return Ok(http);
        }
        let client = crate::source::net::client(
            Duration::from_secs(10),
            Duration::from_secs(10),
            Policy::limited(5),
            self.proxy.as_ref(),
            None,
        )?;
        let _ = self.http.set(HttpSource::from_client(client));
        self.http.get().ok_or_else(|| SourceError::fault(&self.name, "media client unavailable"))
    }
}

impl Source for Provider {
    fn name(&self) -> &str {
        &self.name
    }

    fn claims(&self, identifier: &str) -> bool {
        self.enabled && request::parse(&self.prefixes, &self.routes, identifier).is_some()
    }

    fn owns_search(&self, query: &str) -> bool {
        self.enabled && self.prefixes.iter().any(|prefix| prefix.owns_search(query))
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(self.load_request(reference))
    }
}

impl TrackSource for Provider {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        Box::pin(self.open_stream(track))
    }
}

#[cfg(test)]
#[path = "../../../tests/source_framework.rs"]
mod tests;
