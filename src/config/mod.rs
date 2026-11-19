use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
mod crossfade;
mod plugins;

pub use crossfade::{CrossfadeConfig, CrossfadeCurve};
pub use plugins::{
    ConfigValidationError, FallbackPlaceholder, FallbackProvider, PluginConfig, PluginDirectory,
    resolve_providers,
};

/// Process configuration, read once at startup from a TOML file.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub lavalink: LavalinkConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub sources: player::source::Config,
    #[serde(default)]
    pub crossfade: CrossfadeConfig,
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginConfig>,
    #[serde(default, rename = "plugins")]
    pub plugin_directory: PluginDirectory,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingConfig {
    /// Level for this workspace's crates. At `debug` and `trace`, third-party crates stay at
    /// `info` unless [`levels`](Self::levels) names them.
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Per-target overrides such as `reqwest = "debug"`; these also reopen a default suppression.
    #[serde(default)]
    pub levels: BTreeMap<String, String>,
    #[serde(default)]
    pub format: LogFormat,
    #[serde(default = "default_true")]
    pub timestamps: bool,
    /// Prefix each line with the emitting module target, as in `INFO sources::http: ...`.
    #[serde(default = "default_true")]
    pub show_target: bool,
    #[serde(default = "default_true")]
    pub color: bool,
    #[serde(default = "default_true")]
    pub banner: bool,
    #[serde(default)]
    pub file: Option<LogFileConfig>,
    #[serde(default)]
    pub request: RequestLoggingConfig,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            levels: BTreeMap::new(),
            format: LogFormat::default(),
            timestamps: true,
            show_target: true,
            color: true,
            banner: true,
            file: None,
            request: RequestLoggingConfig::default(),
        }
    }
}

/// One line per finished HTTP request, under `[logging.request]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLoggingConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Log the peer address. Off by default, as in the reference.
    #[serde(default)]
    pub include_client_info: bool,
    /// Log request headers with credentials redacted. Off by default.
    #[serde(default)]
    pub include_headers: bool,
    #[serde(default = "default_true")]
    pub include_query_string: bool,
    #[serde(default = "default_true")]
    pub include_payload: bool,
    #[serde(default = "default_max_payload_length")]
    pub max_payload_length: usize,
    /// Log before the handler runs; the status and payload are then absent.
    #[serde(default)]
    pub before_request: bool,
}

impl Default for RequestLoggingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            include_client_info: false,
            include_headers: false,
            include_query_string: true,
            include_payload: true,
            max_payload_length: default_max_payload_length(),
            before_request: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Compact,
    Pretty,
    Json,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileConfig {
    /// Directory that holds `kairo.log` and its rotated siblings.
    pub path: String,
    /// Rotate once the current file would pass this size; `0` disables size rotation.
    #[serde(default = "default_max_file_size")]
    pub max_file_size: LogSize,
    /// Rotated files to keep beside the current one; `0` discards the old file instead.
    #[serde(default = "default_max_history")]
    pub max_history: usize,
}

/// A byte count written as a number or with a `KB`/`MB`/`GB` suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogSize(pub u64);

impl<'de> Deserialize<'de> for LogSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Text(String),
            Bytes(u64),
        }

        match Value::deserialize(deserializer)? {
            Value::Bytes(bytes) => Ok(Self(bytes)),
            Value::Text(text) => parse_size(&text).map(Self).map_err(serde::de::Error::custom),
        }
    }
}

fn parse_size(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let upper = trimmed.to_ascii_uppercase();
    let (digits, multiplier) = if let Some(value) = upper.strip_suffix("GB") {
        (value, 1024 * 1024 * 1024)
    } else if let Some(value) = upper.strip_suffix("MB") {
        (value, 1024 * 1024)
    } else if let Some(value) = upper.strip_suffix("KB") {
        (value, 1024)
    } else if let Some(value) = upper.strip_suffix('B') {
        (value, 1)
    } else {
        (upper.as_str(), 1)
    };
    let amount = digits.trim().parse::<u64>().map_err(|_| {
        format!("expected a byte count or a value such as 10MB, 1GB (got {text:?})")
    })?;
    amount.checked_mul(multiplier).ok_or_else(|| format!("log size is too large: {text:?}"))
}

fn default_max_file_size() -> LogSize {
    LogSize(1024 * 1024 * 1024)
}

fn default_max_history() -> usize {
    30
}

/// Prometheus exposition settings, mirroring the reference's `metrics.prometheus.*` placement.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MetricsConfig {
    #[serde(default)]
    pub prometheus: PrometheusConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrometheusConfig {
    /// Off by default, as in the reference: scraping is opt-in.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_metrics_endpoint")]
    pub endpoint: String,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self { enabled: false, endpoint: default_metrics_endpoint() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default = "default_address")]
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub http2: Http2Config,
}

/// Spring Boot's `server.http2.enabled`, which is off by default.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Http2Config {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LavalinkConfig {
    #[serde(default)]
    pub server: EngineConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfig {
    /// Auth password, mirroring the reference's `lavalink.server.password` placement.
    #[serde(default)]
    pub password: Option<String>,
    /// Seconds between playerUpdate pushes to a client.
    #[serde(default = "default_player_update_interval")]
    pub player_update_interval: u64,
    /// The reference's native sender queue size. Kairo has no queue with the same shape, so the
    /// value is accepted for config compatibility and warned about once at startup.
    #[serde(default)]
    pub buffer_duration_ms: Option<i64>,
    #[serde(default = "default_frame_buffer_duration")]
    pub frame_buffer_duration_ms: u64,
    #[serde(default = "default_track_stuck_threshold")]
    pub track_stuck_threshold_ms: u64,
    /// Opus encoder quality from 0 to 10; out-of-range values warn and fall back to 10.
    #[serde(default)]
    pub opus_encoding_quality: Option<i64>,
    /// Resampling quality named like the reference's `LOW`, `MEDIUM` and `HIGH`.
    #[serde(default)]
    pub resampling_quality: Option<ResamplingQualityConfig>,
    #[serde(default = "default_true")]
    pub use_seek_ghosting: bool,
    /// Seconds a disconnected session stays resumable.
    #[serde(default = "default_session_timeout")]
    pub session_timeout: u64,
    #[serde(default)]
    pub sources: AudioSourcesConfig,
    #[serde(default)]
    pub filters: BTreeMap<String, bool>,
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
    /// Outbound proxy for the generic HTTP source.
    #[serde(default)]
    pub http_config: Option<HttpProxyConfig>,
    /// IP rotation for outbound source requests, mirroring the reference's route planner.
    #[serde(default)]
    pub ratelimit: Option<RateLimitConfig>,
}

/// Reference `lavalink.server.ratelimit`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitConfig {
    #[serde(default)]
    pub ip_blocks: Vec<String>,
    #[serde(default)]
    pub excluded_ips: Vec<String>,
    #[serde(default = "default_rate_limit_strategy")]
    pub strategy: String,
    #[serde(default = "default_retry_limit")]
    pub retry_limit: i32,
    #[serde(default = "default_true")]
    pub search_triggers_fail: bool,
}

fn default_rate_limit_strategy() -> String {
    String::from("RotateOnBan")
}

const fn default_retry_limit() -> i32 {
    -1
}

/// Reference `lavalink.server.httpConfig`, with the same defaults and blank-host opt-out.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpProxyConfig {
    #[serde(default)]
    pub proxy_host: String,
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,
    #[serde(default)]
    pub proxy_user: String,
    #[serde(default)]
    pub proxy_password: String,
}

impl HttpProxyConfig {
    /// A blank host means the block is present but no proxy is used, exactly as in the reference.
    #[must_use]
    pub fn to_proxy(&self) -> Option<player::source::ProxyConfig> {
        let host = self.proxy_host.trim();
        if host.is_empty() {
            return None;
        }
        let mut proxy =
            player::source::ProxyConfig::new(format!("http://{host}:{}", self.proxy_port));
        if !self.proxy_user.trim().is_empty() {
            proxy = proxy.with_basic_auth(self.proxy_user.clone(), self.proxy_password.clone());
        }
        Some(proxy)
    }
}

impl std::fmt::Debug for HttpProxyConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpProxyConfig")
            .field("proxy_host", &self.proxy_host)
            .field("proxy_port", &self.proxy_port)
            .field("proxy_user", &self.proxy_user)
            .field("proxy_password", &(!self.proxy_password.is_empty()).then_some("<redacted>"))
            .finish()
    }
}

/// Upstream request timeouts, named and defaulted like the reference's `lavalink.server.timeouts`.
/// Reference `ResamplingQuality` values, spelled like the YAML examples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ResamplingQualityConfig {
    #[serde(alias = "low", alias = "Low")]
    Low,
    #[serde(alias = "medium", alias = "Medium")]
    Medium,
    #[serde(alias = "high", alias = "High")]
    High,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeoutsConfig {
    /// Milliseconds allowed to establish a connection.
    #[serde(default = "default_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// Milliseconds allowed to obtain a pooled connection.
    ///
    /// Accepted for Lavalink v4 configuration compatibility; reqwest/hyper connection pooling
    /// has no separate pool-acquisition timeout.
    #[serde(default = "default_timeout_ms")]
    pub connection_request_timeout_ms: u64,
    /// Milliseconds allowed between reads on an open connection.
    #[serde(default = "default_timeout_ms")]
    pub socket_timeout_ms: u64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: default_timeout_ms(),
            connection_request_timeout_ms: default_timeout_ms(),
            socket_timeout_ms: default_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSourcesConfig {
    #[serde(default = "default_true")]
    pub http: bool,
    #[serde(default)]
    pub local: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parse {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("validate {}", path.display())]
    Validation {
        path: PathBuf,
        #[source]
        source: ConfigValidationError,
    },
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = fs::read_to_string(path)
            .map_err(|source| ConfigError::Read { path: path.to_path_buf(), source })?;
        let mut config: Config = toml::from_str(&text)
            .map_err(|source| ConfigError::Parse { path: path.to_path_buf(), source })?;
        config.apply_env_overrides();
        config
            .validate()
            .map_err(|source| ConfigError::Validation { path: path.to_path_buf(), source })?;
        Ok(config)
    }

    /// The configuration a process with no config file runs on: every default, still beaten by
    /// `KAIRO_PASSWORD`, because a secret was never a reason to require a file.
    pub fn defaults() -> Self {
        let mut config = Self::default();
        config.apply_env_overrides();
        config
    }

    fn apply_env_overrides(&mut self) {
        self.override_password(std::env::var("KAIRO_PASSWORD").ok().as_deref());
    }

    fn override_password(&mut self, password: Option<&str>) {
        if let Some(password) = password {
            self.lavalink.server.password = Some(password.to_owned());
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { address: default_address(), port: default_port(), http2: Http2Config::default() }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            password: None,
            player_update_interval: default_player_update_interval(),
            buffer_duration_ms: None,
            frame_buffer_duration_ms: default_frame_buffer_duration(),
            track_stuck_threshold_ms: default_track_stuck_threshold(),
            opus_encoding_quality: None,
            resampling_quality: None,
            use_seek_ghosting: true,
            session_timeout: default_session_timeout(),
            sources: AudioSourcesConfig::default(),
            filters: BTreeMap::new(),
            timeouts: TimeoutsConfig::default(),
            http_config: None,
            ratelimit: None,
        }
    }
}

impl EngineConfig {
    /// Reference behavior for `opusEncodingQuality`: a value outside `0..=10` warns and uses the
    /// maximum quality instead of failing startup.
    #[must_use]
    pub fn opus_quality(&self) -> i32 {
        const MAX: i64 = 10;
        match self.opus_encoding_quality {
            Some(quality) if (0..=MAX).contains(&quality) => i32::try_from(quality).unwrap_or(10),
            Some(quality) => {
                tracing::warn!(
                    value = quality,
                    "opusEncodingQuality is not within 0 to 10; defaulting to 10"
                );
                10
            }
            None => 10,
        }
    }
}

impl Default for AudioSourcesConfig {
    fn default() -> Self {
        Self { http: true, local: false }
    }
}

impl AudioSourcesConfig {
    /// Resolve the built-in source flags, letting `[sources.<name>].enabled` win.
    #[must_use]
    pub fn source_config(&self, tables: &player::source::Config) -> player::source::SourceConfig {
        player::source::SourceConfig {
            http: tables.http.enabled.unwrap_or(self.http),
            local: tables.local.enabled.unwrap_or(self.local),
        }
    }
}

fn default_address() -> String {
    "0.0.0.0".into()
}

fn default_log_level() -> String {
    String::from("info")
}

fn default_metrics_endpoint() -> String {
    "/metrics".into()
}

fn default_max_payload_length() -> usize {
    10_000
}

fn default_port() -> u16 {
    2333
}

fn default_player_update_interval() -> u64 {
    5
}

fn default_track_stuck_threshold() -> u64 {
    10_000
}

fn default_true() -> bool {
    true
}

fn default_frame_buffer_duration() -> u64 {
    5000
}

fn default_session_timeout() -> u64 {
    60
}

fn default_timeout_ms() -> u64 {
    3000
}

fn default_proxy_port() -> u16 {
    3128
}

#[cfg(test)]
#[path = "../tests/config_plugins.rs"]
mod plugin_tests;
#[cfg(test)]
mod tests;
