use std::collections::BTreeMap;
use std::sync::Arc;

use player::ExceptionSeverity;
use player::decode::Millis;
use player::source::{
    HttpSource, LoadFuture, OpenFuture, OpenedSource, ProviderError, Source, SourceError,
    SourceMetadata, SourceReference, SourceResult, TrackSource,
};

use super::mirror::{self, MirrorChain, MirrorSpec};
use kairo_plugin_api::{
    HostError, LoadRequest, LoadResult, LoadedPlugin, Open, OpenRequest, Severity,
    Track as WireTrack, TrackInfo as WireTrackInfo,
};
use player::track::{Playlist, SourceData, Track, TrackBuilder, TrackInfo};

pub(super) struct PluginSource {
    plugin: Arc<LoadedPlugin>,
    name: String,
    claims: SourceClaims,
    http: HttpSource,
    mirror: Arc<MirrorChain>,
}

impl PluginSource {
    pub(super) fn new(
        plugin: Arc<LoadedPlugin>,
        source: &kairo_plugin_api::Source,
        claims: SourceClaims,
        http: HttpSource,
        mirror: Arc<MirrorChain>,
    ) -> Self {
        Self { plugin, name: source.name.clone(), claims, http, mirror }
    }
}

impl TrackSource for PluginSource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        let mut request = match open_request(&self.name, track) {
            Ok(request) => request,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        request.direct = mirror::depth() > 0;
        let plugin = Arc::clone(&self.plugin);
        let http = self.http.clone();
        let mirror = Arc::clone(&self.mirror);
        let identity = PlaybackIdentity {
            source: self.name.clone(),
            reference: track.reference().clone(),
            metadata: track.info().map_or_else(SourceMetadata::default, SourceMetadata::from),
        };
        Box::pin(async move {
            let opened = call_open(Arc::clone(&plugin), &identity.source, &request).await?;
            match opened {
                Open::Url { url, headers, seekable, length_ms: _, content_type } => {
                    open_url(&http, identity, url, headers, seekable, content_type).await
                }
                Open::Mirror { isrc, title, author, length_ms: _ } if !request.direct => {
                    let spec = MirrorSpec { isrc, title, author };
                    match mirror
                        .open(
                            &identity.source,
                            identity.reference.clone(),
                            identity.metadata.clone(),
                            &spec,
                        )
                        .await
                    {
                        Ok(opened) => Ok(Some(opened)),
                        Err(mirror_error) => {
                            let mut direct = request;
                            direct.direct = true;
                            match call_open(plugin, &identity.source, &direct).await? {
                                Open::Url {
                                    url,
                                    headers,
                                    seekable,
                                    length_ms: _,
                                    content_type,
                                } => {
                                    open_url(&http, identity, url, headers, seekable, content_type)
                                        .await
                                }
                                Open::Mirror { .. } => Err(mirror_error),
                                _ => Err(SourceError::fault(
                                    &identity.source,
                                    "plugin returned an unknown direct open type",
                                )),
                            }
                        }
                    }
                }
                Open::Mirror { .. } => Err(SourceError::unsupported(
                    "plugin returned a mirror for a direct open request",
                )),
                _ => Err(SourceError::fault(
                    &identity.source,
                    "plugin returned an unknown open type",
                )),
            }
        })
    }
}

#[derive(Clone)]
struct PlaybackIdentity {
    source: String,
    reference: SourceReference,
    metadata: SourceMetadata,
}

async fn call_open(
    plugin: Arc<LoadedPlugin>,
    source: &str,
    request: &OpenRequest,
) -> Result<Open, SourceError> {
    let request = request.clone();
    let source = source.to_owned();
    plugin.call("track.open", &request).await.map_err(|error| host_error(&source, error))
}

async fn open_url(
    http: &HttpSource,
    identity: PlaybackIdentity,
    url: String,
    headers: kairo_plugin_api::Headers,
    seekable: Option<bool>,
    content_type: Option<String>,
) -> Result<Option<OpenedSource>, SourceError> {
    let url =
        reqwest::Url::parse(&url).map_err(|error| SourceError::fault(&identity.source, error))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SourceError::unsupported("plugin media URL must use http or https"));
    }
    let headers = headers.into_iter().collect::<Vec<_>>();
    let Some(opened) =
        http.open_with_headers(SourceReference::new(url.to_string()), &headers).await?
    else {
        return Ok(None);
    };
    let mut hints = opened.hints().clone();
    if let Some(content_type) = content_type.filter(|value| !value.is_empty()) {
        hints.mime_type = Some(content_type);
    }
    let content_length = opened.content_length();
    let seekable = seekable.unwrap_or_else(|| opened.is_seekable());
    Ok(Some(
        opened
            .with_stream_info(hints, content_length, seekable)
            .with_source_name(identity.source)
            .with_reference(identity.reference)
            .with_metadata(identity.metadata),
    ))
}

impl Source for PluginSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn claims(&self, identifier: &str) -> bool {
        self.claims.matches(identifier)
    }

    fn owns_search(&self, query: &str) -> bool {
        self.claims.prefixes.iter().any(|prefix| query.starts_with(prefix))
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        let identifier = reference.identifier().to_owned();
        if !self.claims.matches(&identifier) {
            return Box::pin(async { Ok(SourceResult::NotMine) });
        }
        let plugin = Arc::clone(&self.plugin);
        let source = self.name.clone();
        Box::pin(async move {
            let request = LoadRequest { source: source.clone(), identifier: identifier.clone() };
            let result = plugin
                .call("source.load", &request)
                .await
                .map_err(|error| host_error(&source, error))?;
            convert_load(&source, &identifier, result)
        })
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceClaims {
    pub(super) prefixes: Vec<String>,
    pub(super) hosts: Vec<String>,
}

impl SourceClaims {
    pub(super) fn new(source: &kairo_plugin_api::Source) -> Self {
        Self {
            prefixes: source.prefixes.iter().map(|prefix| format!("{prefix}:")).collect(),
            hosts: source.hosts.iter().map(|host| host.to_ascii_lowercase()).collect(),
        }
    }

    fn matches(&self, identifier: &str) -> bool {
        if self.prefixes.iter().any(|prefix| identifier.starts_with(prefix)) {
            return true;
        }
        let Ok(url) = reqwest::Url::parse(identifier) else {
            return false;
        };
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        self.hosts.iter().any(|claim| {
            host.eq_ignore_ascii_case(claim)
                || host.strip_suffix(claim).is_some_and(|prefix| prefix.ends_with('.'))
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistrationError {
    #[error("plugin source {source_name:?} has invalid prefix {prefix:?}")]
    InvalidPrefix { source_name: String, prefix: String },
    #[error("plugin source {source_name:?} has invalid host {host:?}")]
    InvalidHost { source_name: String, host: String },
    #[error("plugin {kind} {value:?} is claimed by both {first:?} and {second:?}")]
    Collision { kind: &'static str, value: String, first: String, second: String },
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Source(#[from] SourceError),
}

pub(super) struct ClaimRegistry {
    names: BTreeMap<String, String>,
    prefixes: BTreeMap<String, String>,
    hosts: Vec<(String, String)>,
}

impl ClaimRegistry {
    pub(super) fn new<'a>(reserved: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            names: reserved
                .into_iter()
                .map(|name| (name.to_owned(), String::from("built-in")))
                .collect(),
            prefixes: BTreeMap::new(),
            hosts: Vec::new(),
        }
    }

    pub(super) fn add(
        &mut self,
        source: &kairo_plugin_api::Source,
    ) -> Result<SourceClaims, RegistrationError> {
        collision(&mut self.names, "source name", &source.name, &source.name)?;
        let claims = validate_claims(source)?;
        for prefix in &claims.prefixes {
            collision(&mut self.prefixes, "prefix", prefix, &source.name)?;
        }
        for host in &claims.hosts {
            if let Some((claimed, owner)) =
                self.hosts.iter().find(|(claimed, _)| hosts_overlap(claimed, host))
            {
                return Err(RegistrationError::Collision {
                    kind: "host",
                    value: format!("{claimed} / {host}"),
                    first: owner.clone(),
                    second: source.name.clone(),
                });
            }
            self.hosts.push((host.clone(), source.name.clone()));
        }
        Ok(claims)
    }
}

pub(super) fn validate_claims(
    source: &kairo_plugin_api::Source,
) -> Result<SourceClaims, RegistrationError> {
    for prefix in &source.prefixes {
        if !valid_label(prefix) {
            return Err(RegistrationError::InvalidPrefix {
                source_name: source.name.clone(),
                prefix: prefix.clone(),
            });
        }
    }
    for host in &source.hosts {
        if !valid_host(host) {
            return Err(RegistrationError::InvalidHost {
                source_name: source.name.clone(),
                host: host.clone(),
            });
        }
    }
    Ok(SourceClaims::new(source))
}

pub(super) fn hosts_overlap(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
        || left.strip_suffix(right).is_some_and(|prefix| prefix.ends_with('.'))
        || right.strip_suffix(left).is_some_and(|prefix| prefix.ends_with('.'))
}

fn collision(
    claims: &mut BTreeMap<String, String>,
    kind: &'static str,
    value: &str,
    owner: &str,
) -> Result<(), RegistrationError> {
    if let Some(first) = claims.insert(value.to_owned(), owner.to_owned()) {
        return Err(RegistrationError::Collision {
            kind,
            value: value.to_owned(),
            first,
            second: owner.to_owned(),
        });
    }
    Ok(())
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.split('.').all(|part| {
            !part.is_empty()
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn convert_load(
    source: &str,
    identifier: &str,
    result: LoadResult,
) -> Result<SourceResult, SourceError> {
    match result {
        LoadResult::Track(track) => convert_track(source, track).map(SourceResult::Track),
        LoadResult::Search(tracks) => {
            let tracks = convert_tracks(source, tracks)?;
            if tracks.is_empty() {
                Ok(SourceResult::Empty)
            } else {
                Ok(SourceResult::Playlist(Playlist::search(
                    format!("{source} Search: {identifier}"),
                    tracks,
                )))
            }
        }
        LoadResult::Playlist(playlist) => {
            if playlist.name.is_empty() {
                return Err(SourceError::fault(source, "plugin playlist name is empty"));
            }
            let tracks = convert_tracks(source, playlist.tracks)?;
            let mut result = Playlist::new(playlist.name, tracks);
            if playlist.selected_track < -1 {
                return Err(SourceError::fault(source, "plugin selectedTrack is below -1"));
            }
            if let Ok(index) = usize::try_from(playlist.selected_track) {
                result = result
                    .with_selected_track(index)
                    .map_err(|error| SourceError::fault(source, error))?;
            }
            Ok(SourceResult::Playlist(result))
        }
        LoadResult::Empty(()) => Ok(SourceResult::Empty),
        _ => Err(SourceError::fault(source, "plugin returned an unknown load type")),
    }
}

fn convert_tracks(source: &str, tracks: Vec<WireTrack>) -> Result<Vec<Track>, SourceError> {
    tracks.into_iter().map(|track| convert_track(source, track)).collect()
}

fn convert_track(source: &str, track: WireTrack) -> Result<Track, SourceError> {
    let (info, plugin_info, plugin_data) = track.into_parts();
    if info.source_name != source {
        return Err(SourceError::fault(
            source,
            format!("plugin track names source {:?}", info.source_name),
        ));
    }
    if info.identifier.is_empty() {
        return Err(SourceError::fault(source, "plugin track identifier is empty"));
    }
    let reference = SourceReference::new(info.identifier.clone());
    let data =
        plugin_data.map_or(SourceData::None, |data| SourceData::Opaque(data.into_bytes().into()));
    let info = TrackInfo {
        identifier: info.identifier,
        is_seekable: info.is_seekable,
        author: info.author,
        length: (!info.is_stream).then(|| Millis::new(info.length)),
        is_stream: info.is_stream,
        title: info.title,
        uri: info.uri,
        source_name: info.source_name,
        artwork_url: info.artwork_url,
        isrc: info.isrc,
    };
    Ok(TrackBuilder::new(reference, info).source_data(data).plugin_info(plugin_info).build())
}

fn open_request(source: &str, track: &Track) -> Result<OpenRequest, SourceError> {
    let Some(info) = track.info() else {
        return Err(SourceError::UnresolvedTrack);
    };
    if info.source_name != source {
        return Err(SourceError::UnknownSource { name: info.source_name.clone() });
    }
    let wire_info = WireTrackInfo {
        identifier: info.identifier.clone(),
        title: info.title.clone(),
        author: info.author.clone(),
        length: info.length.map_or(0, Millis::get),
        is_stream: info.is_stream,
        is_seekable: info.is_seekable,
        uri: info.uri.clone(),
        artwork_url: info.artwork_url.clone(),
        isrc: info.isrc.clone(),
        source_name: info.source_name.clone(),
    };
    let mut wire = WireTrack::new(wire_info);
    wire.plugin_info = track.plugin_info().clone();
    match track.source_data() {
        SourceData::None => {}
        SourceData::Opaque(data) => {
            let data = std::str::from_utf8(data)
                .map_err(|_| SourceError::fault(source, "pluginData is not UTF-8"))?;
            wire = wire.plugin_data(data).map_err(|error| SourceError::fault(source, error))?;
        }
        _ => return Err(SourceError::fault(source, "track has incompatible source data")),
    }
    Ok(OpenRequest { source: source.to_owned(), track: wire, direct: false })
}

fn host_error(source: &str, error: HostError) -> SourceError {
    match error {
        HostError::Call { error, .. } => {
            let severity = match error.severity {
                Severity::Common => ExceptionSeverity::Common,
                Severity::Suspicious => ExceptionSeverity::Suspicious,
                Severity::Fault => ExceptionSeverity::Fault,
                _ => ExceptionSeverity::Fault,
            };
            SourceError::load_failed(error.message, severity)
        }
        error => SourceError::fault(source, error),
    }
}

#[cfg(test)]
#[path = "../tests/plugins_adapter.rs"]
mod tests;
