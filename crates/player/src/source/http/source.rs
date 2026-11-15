use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use reqwest::Client;
use reqwest::Url;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::redirect::Policy;

use kairo_core::ExceptionSeverity;
use kairo_core::track::{Track, UNKNOWN_TITLE};
use kairo_core::{
    OpenedSource, SourceHints, SourceMetadata, SourceMetadataUpdates, SourceReference,
};

use super::hls::HlsStream;
use super::mpegts::{TsToAdts, looks_like_ts};
use super::stream::{HttpStream, read_bounded, response_body};
use crate::source::engine::{LoadFuture, OpenFuture, Source, SourceResult, TrackSource};
use crate::source::error::SourceError;
use crate::source::net::LocalAddressProvider;
use crate::source::proxy::ProxyConfig;
pub const HTTP_SOURCE: &str = "http";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OPEN_REDIRECTS: usize = 5;
const PLAYLIST_SCAN_BYTES: usize = 1_000;
const MAX_PLAYLIST_BYTES: usize = 1024 * 1024;

/// Direct HTTP(S) media with redirects returned to the ordered source engine.
#[derive(Clone)]
pub struct HttpSource {
    clients: Clients,
}

#[derive(Clone)]
enum Clients {
    /// One prepared client, used by providers that own their own routing.
    Fixed(Client),
    /// A client rebuilt whenever the route planner moves to another local address.
    Dynamic(Arc<DynamicClients>),
}

struct DynamicClients {
    connect: Duration,
    read: Duration,
    proxy: Option<ProxyConfig>,
    local_address: LocalAddressProvider,
    cached: Mutex<Option<(Option<IpAddr>, Client)>>,
}

impl DynamicClients {
    fn get(&self) -> Result<Client, SourceError> {
        let local = (self.local_address)();
        let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((address, client)) = cached.as_ref()
            && *address == local
        {
            return Ok(client.clone());
        }
        let client = crate::source::net::client(
            self.connect,
            self.read,
            Policy::none(),
            self.proxy.as_ref(),
            local,
        )?;
        *cached = Some((local, client.clone()));
        Ok(client)
    }
}

impl HttpSource {
    pub fn new() -> Result<Self, SourceError> {
        Self::with_timeouts(REQUEST_TIMEOUT, REQUEST_TIMEOUT)
    }

    /// Build with explicit connect and read timeouts; zero leaves that side unbounded.
    pub fn with_timeouts(connect: Duration, read: Duration) -> Result<Self, SourceError> {
        Self::with_options(connect, read, None, None)
    }

    /// Build with a caller-supplied outbound proxy and the default request timeouts.
    pub fn with_proxy(
        proxy: Option<&ProxyConfig>,
        local_address: Option<LocalAddressProvider>,
    ) -> Result<Self, SourceError> {
        Self::with_options(REQUEST_TIMEOUT, REQUEST_TIMEOUT, proxy, local_address)
    }

    /// Build with explicit timeouts, an optional outbound proxy and an optional local-address
    /// provider. A provider makes the source bind each request to the address it reports.
    pub fn with_options(
        connect: Duration,
        read: Duration,
        proxy: Option<&ProxyConfig>,
        local_address: Option<LocalAddressProvider>,
    ) -> Result<Self, SourceError> {
        Ok(Self {
            clients: match local_address {
                Some(local_address) => Clients::Dynamic(Arc::new(DynamicClients {
                    connect,
                    read,
                    proxy: proxy.cloned(),
                    local_address,
                    cached: Mutex::new(None),
                })),
                None => Clients::Fixed(crate::source::net::client(
                    connect,
                    read,
                    Policy::none(),
                    proxy,
                    None,
                )?),
            },
        })
    }

    /// Use a prepared client. Redirects stay disabled so the source engine owns them.
    #[must_use]
    pub fn from_client(client: Client) -> Self {
        Self { clients: Clients::Fixed(client) }
    }

    fn client(&self) -> Result<Client, SourceError> {
        match &self.clients {
            Clients::Fixed(client) => Ok(client.clone()),
            Clients::Dynamic(dynamic) => dynamic.get(),
        }
    }

    async fn request_reference(
        &self,
        reference: &SourceReference,
        headers: &[(String, String)],
    ) -> Result<SourceResult, SourceError> {
        let Some(reference) = crate::source::net::direct_reference(reference) else {
            return Ok(SourceResult::NotMine);
        };
        self.request(reference, headers).await
    }

    async fn request(
        &self,
        reference: SourceReference,
        headers: &[(String, String)],
    ) -> Result<SourceResult, SourceError> {
        let identifier = reference.identifier().to_owned();
        let client = self.client()?;
        let mut request = client.get(reference.identifier());
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = request.send().await.map_err(|source| SourceError::HttpRequest {
            identifier: identifier.clone(),
            source,
        })?;
        let status = response.status().as_u16();

        if matches!(status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = response.headers().get(LOCATION)
        {
            let location = location.to_str().map_err(|_| SourceError::InvalidRedirect {
                identifier: identifier.clone(),
                location: String::from("<non-UTF-8>"),
            })?;
            let target =
                response.url().join(location).map_err(|_| SourceError::InvalidRedirect {
                    identifier: identifier.clone(),
                    location: location.to_owned(),
                })?;
            let target: String = target.into();
            return Ok(SourceResult::Redirect(SourceReference::new(target)));
        }

        if status == 404 {
            return Ok(SourceResult::NotMine);
        }
        if !matches!(status, 200 | 203 | 206) {
            return Err(SourceError::UnplayableHttp { identifier, status });
        }

        let content_length = response.content_length();
        let mime_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let header = |name| {
            response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_owned)
        };
        let metadata = SourceMetadata {
            title: reference
                .title()
                .map(str::to_owned)
                .or_else(|| header("icy-description"))
                .or_else(|| header("icy-url")),
            author: header("icy-name"),
            artwork_url: None,
            isrc: None,
        };
        let url = response.url().clone();
        let mut hints = SourceHints { mime_type, file_extension: extension(&url) };
        let mut stream =
            HttpStream::new(client.clone(), url.clone(), response_body(response), content_length);
        stream
            .prime(PLAYLIST_SCAN_BYTES)
            .await
            .map_err(|source| SourceError::HttpBody { identifier: identifier.clone(), source })?;
        if let Some(format) = detect(stream.prefix(), &hints) {
            let bytes =
                read_bounded(Vec::new(), Box::pin(stream), MAX_PLAYLIST_BYTES).await.map_err(
                    |source| SourceError::HttpBody { identifier: identifier.clone(), source },
                )?;
            let text = String::from_utf8(bytes).map_err(|_| {
                SourceError::load_failed(
                    "The playlist file is not valid UTF-8.",
                    ExceptionSeverity::Suspicious,
                )
            })?;
            return match parse(format, &text, &url) {
                Some(PlaylistTarget::Redirect(reference)) => Ok(SourceResult::Redirect(reference)),
                Some(PlaylistTarget::Hls) => {
                    hints.mime_type = Some(String::from("audio/aac"));
                    hints.file_extension = Some(String::from("aac"));
                    let hls = HlsStream::new(client.clone(), url, text);
                    let updates: SourceMetadataUpdates =
                        Arc::new(Mutex::new(SourceMetadata::default()));
                    let stream = TsToAdts::with_metadata(hls, updates.clone());
                    Ok(SourceResult::Opened(
                        OpenedSource::read_only(HTTP_SOURCE, reference, hints, None, stream)
                            .with_metadata(metadata)
                            .with_metadata_updates(updates),
                    ))
                }
                None => Err(SourceError::load_failed(
                    "The playlist file contains no links.",
                    ExceptionSeverity::Common,
                )),
            };
        }

        let is_mpeg_ts = is_mpeg_ts(&hints, stream.prefix());
        if is_mpeg_ts {
            hints.mime_type = Some(String::from("audio/aac"));
            hints.file_extension = Some(String::from("aac"));
            let updates: SourceMetadataUpdates = Arc::new(Mutex::new(SourceMetadata::default()));
            let stream = TsToAdts::with_metadata(stream, updates.clone());
            return Ok(SourceResult::Opened(
                OpenedSource::read_only(HTTP_SOURCE, reference, hints, None, stream)
                    .with_metadata(metadata)
                    .with_metadata_updates(updates),
            ));
        }
        let opened = match content_length {
            Some(_) => {
                OpenedSource::seekable(HTTP_SOURCE, reference, hints, content_length, stream)
            }
            None => OpenedSource::read_only(HTTP_SOURCE, reference, hints, None, stream),
        }
        .with_metadata(metadata);
        Ok(SourceResult::Opened(opened))
    }
}

fn extension(url: &Url) -> Option<String> {
    url.path_segments()?
        .next_back()?
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
}

fn is_mpeg_ts(hints: &SourceHints, prefix: &[u8]) -> bool {
    hints.file_extension.as_deref().is_some_and(|extension| extension.eq_ignore_ascii_case("ts"))
        || hints.mime_type.as_deref().is_some_and(|mime| {
            mime.eq_ignore_ascii_case("video/mp2t") || mime.eq_ignore_ascii_case("audio/mp2t")
        })
        || looks_like_ts(prefix)
}

impl Source for HttpSource {
    fn name(&self) -> &str {
        HTTP_SOURCE
    }

    fn claims(&self, identifier: &str) -> bool {
        crate::source::net::is_direct(identifier)
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(self.request_reference(reference, &[]))
    }
}

impl HttpSource {
    /// Open one reference with extra request headers and no metadata step.
    pub async fn open_with_headers(
        &self,
        mut reference: SourceReference,
        headers: &[(String, String)],
    ) -> Result<Option<OpenedSource>, SourceError> {
        let identifier = reference.identifier().to_owned();
        for _ in 0..MAX_OPEN_REDIRECTS {
            match self.request_reference(&reference, headers).await? {
                SourceResult::Opened(opened) => return Ok(Some(opened)),
                SourceResult::NotMine | SourceResult::Empty => return Ok(None),
                SourceResult::Redirect(next)
                    if crate::source::net::is_direct(next.identifier()) =>
                {
                    reference = next;
                }
                SourceResult::Redirect(next) => {
                    return Err(SourceError::InvalidRedirect {
                        identifier,
                        location: next.identifier().to_owned(),
                    });
                }
                _ => {
                    return Err(SourceError::InvalidPlaybackResult {
                        name: String::from(HTTP_SOURCE),
                    });
                }
            }
        }
        Err(SourceError::load_failed(
            format!("URL {identifier} exceeded {MAX_OPEN_REDIRECTS} redirects"),
            ExceptionSeverity::Suspicious,
        ))
    }
}

impl TrackSource for HttpSource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        Box::pin(self.open_with_headers(track.reference().clone(), &[]))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistFormat {
    M3u,
    Pls,
    Plain,
}

#[derive(Debug, PartialEq, Eq)]
enum PlaylistTarget {
    Redirect(SourceReference),
    Hls,
}

fn detect(bytes: &[u8], hints: &SourceHints) -> Option<PlaylistFormat> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.starts_with("#EXTM3U") || text.starts_with("#EXTINF") {
        return Some(PlaylistFormat::M3u);
    }
    if is_pls_header(text.as_bytes()) {
        return Some(PlaylistFormat::Pls);
    }
    if is_url(text.lines().next().unwrap_or_default()) {
        return Some(PlaylistFormat::Plain);
    }

    let extension = hints.file_extension.as_deref().map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("m3u" | "m3u8") => return Some(PlaylistFormat::M3u),
        Some("pls") => return Some(PlaylistFormat::Pls),
        _ => {}
    }
    let mime = hints.mime_type.as_deref().map(str::to_ascii_lowercase);
    match mime.as_deref() {
        Some(
            "application/vnd.apple.mpegurl"
            | "application/x-mpegurl"
            | "audio/mpegurl"
            | "audio/x-mpegurl",
        ) => Some(PlaylistFormat::M3u),
        Some("audio/x-scpls") => Some(PlaylistFormat::Pls),
        Some("text/plain") => Some(PlaylistFormat::Plain),
        _ => None,
    }
}

fn parse(format: PlaylistFormat, text: &str, _base: &Url) -> Option<PlaylistTarget> {
    match format {
        PlaylistFormat::M3u => parse_m3u(text),
        PlaylistFormat::Pls => parse_pls(text),
        PlaylistFormat::Plain => parse_plain(text),
    }
}

fn parse_m3u(text: &str) -> Option<PlaylistTarget> {
    let mut title = None;
    let mut hls_entry_pending = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("#EXT-X-STREAM-INF") {
            hls_entry_pending = true;
        } else if let Some(info) = line.strip_prefix("#EXTINF") {
            title = info.split_once(',').map(|(_, title)| title.to_owned());
            hls_entry_pending = true;
        } else if line.starts_with('#') || line.is_empty() {
            continue;
        } else if hls_entry_pending {
            return Some(PlaylistTarget::Hls);
        } else if is_url(line) {
            let reference = title.take().map_or_else(
                || SourceReference::new(line),
                |title| SourceReference::with_title(line, title),
            );
            return Some(PlaylistTarget::Redirect(reference));
        } else {
            title = None;
        }
    }
    None
}

fn parse_pls(text: &str) -> Option<PlaylistTarget> {
    let mut titles = Vec::new();
    let mut files = Vec::new();
    for line in text.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(index) = key.strip_prefix("Title") {
            titles.push((index, value));
        } else if let Some(index) = key.strip_prefix("File")
            && is_url(value)
        {
            files.push((index, value));
        }
    }
    let (index, url) = files.first()?;
    let title = titles
        .iter()
        .find_map(|(title_index, title)| (title_index == index).then_some(*title))
        .unwrap_or(UNKNOWN_TITLE);
    Some(PlaylistTarget::Redirect(SourceReference::with_title(*url, title)))
}

fn parse_plain(text: &str) -> Option<PlaylistTarget> {
    text.lines()
        .find(|line| is_url(line))
        .map(|url| PlaylistTarget::Redirect(SourceReference::new(url)))
}

fn is_pls_header(bytes: &[u8]) -> bool {
    bytes.len() >= 10 && bytes[0] == b'[' && bytes[2..10].eq_ignore_ascii_case(b"laylist]")
}

fn is_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://") || value.starts_with("icy://")
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum HlsManifest {
    Master(Url),
    Media { segments: Vec<HlsSegment>, ended: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HlsSegment {
    pub(super) url: Url,
    pub(super) duration: Option<Duration>,
}

pub(super) fn parse_hls(text: &str, base: &Url) -> Result<HlsManifest, &'static str> {
    let mut variant_pending = false;
    let mut duration = None;
    let mut segments = Vec::new();
    let mut ended = false;

    for line in text.lines().map(str::trim) {
        if line.starts_with("#EXT-X-STREAM-INF") {
            variant_pending = true;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            duration = value
                .split_once(',')
                .map_or(value, |(duration, _)| duration)
                .parse::<f64>()
                .ok()
                .filter(|duration| duration.is_finite() && *duration >= 0.0)
                .map(Duration::from_secs_f64);
        } else if line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if line.starts_with("#EXT-X-MAP") {
            return Err("fragmented MP4 HLS segments are not supported");
        } else if line.starts_with("#EXT-X-BYTERANGE") {
            return Err("byte-range HLS segments are not supported");
        } else if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            if !value.split(',').any(|field| field == "METHOD=NONE") {
                return Err("encrypted HLS segments are not supported");
            }
        } else if line.starts_with('#') || line.is_empty() {
            continue;
        } else {
            let url = base.join(line).map_err(|_| "HLS manifest contains an invalid URL")?;
            if variant_pending {
                return Ok(HlsManifest::Master(url));
            }
            segments.push(HlsSegment { url, duration: duration.take() });
        }
    }

    Ok(HlsManifest::Media { segments, ended })
}

#[cfg(test)]
#[path = "../../../tests/source_playlists.rs"]
mod playlist_tests;
