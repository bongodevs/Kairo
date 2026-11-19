//! JSON messages shared by plugins and the host.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::ABI_VERSION;

/// Largest opaque source payload carried by one track.
pub const MAX_PLUGIN_DATA_BYTES: usize = 4 * 1024;

/// Request and response headers as JSON object fields.
pub type Headers = BTreeMap<String, String>;

/// Plugin identity and the sources it provides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub abi: u32,
    #[serde(default)]
    pub sources: Vec<Source>,
}

/// One source advertised by a plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    #[serde(default)]
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// Input to `source.load`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadRequest {
    pub source: String,
    pub identifier: String,
}

/// Client-visible metadata for one resolved track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    pub identifier: String,
    pub title: String,
    pub author: String,
    pub length: u64,
    pub is_stream: bool,
    pub is_seekable: bool,
    pub uri: Option<String>,
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
    pub source_name: String,
}

/// One track returned by a load call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub info: TrackInfo,
    #[serde(default)]
    pub plugin_info: Map<String, Value>,
    #[serde(default, deserialize_with = "deserialize_plugin_data")]
    plugin_data: Option<String>,
}

impl Track {
    #[must_use]
    pub fn new(info: TrackInfo) -> Self {
        Self { info, plugin_info: Map::new(), plugin_data: None }
    }

    #[must_use]
    pub fn plugin_info(mut self, plugin_info: Map<String, Value>) -> Self {
        self.plugin_info = plugin_info;
        self
    }

    pub fn plugin_data(mut self, plugin_data: impl Into<String>) -> Result<Self, PluginError> {
        let plugin_data = plugin_data.into();
        if plugin_data.len() > MAX_PLUGIN_DATA_BYTES {
            return Err(PluginError {
                severity: Severity::Fault,
                message: plugin_data_message(plugin_data.len()),
                unavailable: false,
            });
        }
        self.plugin_data = Some(plugin_data);
        Ok(self)
    }

    #[must_use]
    pub fn plugin_data_ref(&self) -> Option<&str> {
        self.plugin_data.as_deref()
    }

    #[must_use]
    pub fn into_parts(self) -> (TrackInfo, Map<String, Value>, Option<String>) {
        (self.info, self.plugin_info, self.plugin_data)
    }
}

impl From<TrackInfo> for Track {
    fn from(info: TrackInfo) -> Self {
        Self::new(info)
    }
}

/// Playlist data returned by a load call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub name: String,
    pub selected_track: i32,
    pub tracks: Vec<Track>,
}

/// Result of `source.load`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "loadType", content = "data", rename_all = "lowercase")]
#[non_exhaustive]
pub enum LoadResult {
    Track(Track),
    Search(Vec<Track>),
    Playlist(Playlist),
    Empty(()),
}

/// Input to `track.open`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub source: String,
    pub track: Track,
    /// Ask for a direct URL instead of a mirror request. The host sets this when it opens a
    /// provider's fallback track.
    #[serde(default, skip_serializing_if = "is_false")]
    pub direct: bool,
}

/// A final media URL or a request to resolve a mirror.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[non_exhaustive]
pub enum Open {
    Url {
        url: String,
        #[serde(default)]
        headers: Headers,
        seekable: Option<bool>,
        #[serde(rename = "lengthMs")]
        length_ms: Option<u64>,
        #[serde(rename = "contentType")]
        content_type: Option<String>,
    },
    Mirror {
        isrc: Option<String>,
        title: String,
        author: String,
        #[serde(rename = "lengthMs")]
        length_ms: Option<u64>,
    },
}

/// Client-facing failure severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Severity {
    Common,
    Suspicious,
    Fault,
}

/// Failure body shared by plugin calls and native sources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unavailable: bool,
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

impl PluginError {
    #[must_use]
    pub fn common(message: impl Into<String>) -> Self {
        Self::new(Severity::Common, message, false)
    }

    #[must_use]
    pub fn suspicious(message: impl Into<String>) -> Self {
        Self::new(Severity::Suspicious, message, false)
    }

    #[must_use]
    pub fn fault(message: impl Into<String>) -> Self {
        Self::new(Severity::Fault, message, false)
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(Severity::Suspicious, message, true)
    }

    fn new(severity: Severity, message: impl Into<String>, unavailable: bool) -> Self {
        Self { severity, message: message.into(), unavailable }
    }
}

/// HTTP method accepted by the host callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[non_exhaustive]
pub enum HttpMethod {
    Delete,
    Get,
    Head,
    Patch,
    Post,
    Put,
}

/// JSON request passed to the host HTTP callback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    #[serde(default)]
    pub headers: Headers,
    pub body: Option<String>,
}

/// JSON response returned by the host HTTP callback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Headers,
    pub body: String,
}

impl Manifest {
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self { name: name.into(), version: version.into(), abi: ABI_VERSION, sources: Vec::new() }
    }

    #[must_use]
    pub fn source(mut self, source: Source) -> Self {
        self.sources.push(source);
        self
    }
}

impl Source {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), prefixes: Vec::new(), hosts: Vec::new() }
    }

    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefixes.push(prefix.into());
        self
    }

    #[must_use]
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.hosts.push(host.into());
        self
    }
}

impl LoadRequest {
    #[must_use]
    pub fn search(&self, prefix: &str) -> Option<&str> {
        self.identifier.strip_prefix(prefix).filter(|query| !query.is_empty())
    }
}

impl LoadResult {
    #[must_use]
    pub const fn empty() -> Self {
        Self::Empty(())
    }
}

impl TrackInfo {
    #[must_use]
    pub fn new(
        identifier: impl Into<String>,
        title: impl Into<String>,
        author: impl Into<String>,
        source_name: impl Into<String>,
        length: u64,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            title: title.into(),
            author: author.into(),
            length,
            is_stream: false,
            is_seekable: true,
            uri: None,
            artwork_url: None,
            isrc: None,
            source_name: source_name.into(),
        }
    }
}

impl Open {
    #[must_use]
    pub fn url(url: impl Into<String>) -> Self {
        Self::Url {
            url: url.into(),
            headers: Headers::new(),
            seekable: None,
            length_ms: None,
            content_type: None,
        }
    }

    #[must_use]
    pub fn mirror(title: impl Into<String>, author: impl Into<String>) -> Self {
        Self::Mirror { isrc: None, title: title.into(), author: author.into(), length_ms: None }
    }
}

fn deserialize_plugin_data<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = value.as_ref().filter(|value| value.len() > MAX_PLUGIN_DATA_BYTES) {
        return Err(D::Error::custom(plugin_data_message(value.len())));
    }
    Ok(value)
}

fn plugin_data_message(actual: usize) -> String {
    format!("pluginData is {actual} bytes; maximum is {MAX_PLUGIN_DATA_BYTES}")
}

const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
#[path = "../tests/model.rs"]
mod tests;
