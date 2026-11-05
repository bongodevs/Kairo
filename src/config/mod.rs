//! Configuration parsed from `config.toml` at startup.

mod crossfade;
mod filters;
mod lavalink;
mod logging;
mod lyrics;
mod metrics;
mod server;

pub use ::sources::SourcesConfig;
pub use crossfade::{CrossfadeConfig, CrossfadeCurve};
pub use filters::FiltersToggleConfig;
pub use lavalink::{
    HttpConfig, LavalinkConfig, LavalinkServerConfig, RatelimitConfig, RatelimitStrategy,
    ResamplingQuality,
};
pub use logging::{LogFileConfig, LogFormat, LogRotation, LoggingConfig, RequestLoggingConfig};
pub use lyrics::LyricsServerConfig;
pub use metrics::{MetricsConfig, PrometheusConfig};
pub use server::{Http2Config, ServerConfig};

use serde::Deserialize;

use player::AudioConfiguration;

const DEFAULT_CONFIG: &str = "config.toml";
// Top-level keys the config recognises; anything else is a likely typo. Kept in step with the
// fields of `Config` below.
const KNOWN_SECTIONS: &[&str] = &[
    "server",
    "lavalink",
    "logging",
    "sources",
    "crossfade",
    "lyrics",
    "metrics",
];

// Warn, before logging is up, about unrecognised top-level sections. Serde drops unknown keys
// rather than failing, so this is the only hint a typo'd section was ignored.
fn warn_unknown_sections<'a>(keys: impl Iterator<Item = &'a str>) {
    for key in keys {
        if !KNOWN_SECTIONS.contains(&key) {
            eprintln!("warning: unknown config section '{key}' ignored");
        }
    }
}

/// The whole config file. Every block is optional and falls back to its own defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The HTTP listener, `server.*`.
    pub server: ServerConfig,
    /// Node settings, `lavalink.*`.
    pub lavalink: LavalinkConfig,
    /// This server's own logging, `logging.*`.
    pub logging: LoggingConfig,
    /// Every source toggle and per-source setting, `sources.*`.
    pub sources: SourcesConfig,
    /// Track transition defaults, `crossfade.*`.
    pub crossfade: CrossfadeConfig,
    /// Lyrics providers, `lyrics.*`.
    pub lyrics: LyricsServerConfig,
    /// Prometheus metrics, `metrics.*`.
    pub metrics: MetricsConfig,
}

impl Config {
    /// Read the file named by the first argument, then `KAIRO_CONFIG`, then `config.toml`.
    ///
    /// Panics if the file cannot be read or parsed, since there is nothing to serve without it.
    /// The `PORT` environment variable, when set, overrides the file's `server.port`.
    /// `KAIRO_YOUTUBE_URL` / `KAIRO_YOUTUBE_TOKEN` likewise override the remote resolver's address.
    pub fn new() -> Self {
        let path = std::env::args()
            .nth(1)
            .or_else(|| std::env::var("KAIRO_CONFIG").ok())
            .unwrap_or_else(|| DEFAULT_CONFIG.to_string());
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to read config '{}': {}", path, e));
        let mut config = Self::from_toml(&content)
            .unwrap_or_else(|e| panic!("Failed to parse config '{}': {}", path, e));
        config.set_port(std::env::var("PORT").ok().as_deref());
        config.sources.youtube.remote.apply_env();
        config
    }

    fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        if let Ok(table) = toml::from_str::<toml::Table>(content) {
            warn_unknown_sections(table.keys().map(String::as_str));
        }
        toml::from_str(content)
    }

    // `PORT` is the container convention: it lets several instances share one config file and
    // differ only in the port they bind. Parsed separately from the env read so it stays testable.
    fn set_port(&mut self, port: Option<&str>) {
        let Some(port) = port else {
            return;
        };
        self.server.port = port.parse().unwrap_or_else(|_| {
            panic!("Invalid PORT '{}': expected a number from 0 to 65535", port)
        });
    }

    /// Build the engine-wide [`AudioConfiguration`] from these settings.
    pub fn audio_configuration(&self) -> AudioConfiguration {
        let server = &self.lavalink.server;
        let mut config = AudioConfiguration {
            resampling_quality: server.resampling_quality.to_engine(),
            track_stuck_threshold_ms: server.track_stuck_threshold_ms,
            ..AudioConfiguration::default()
        };
        config.set_opus_encoding_quality(server.opus_encoding_quality);
        config.set_opus_bitrate(server.opus_bitrate);
        config
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn example_toml_parses() {
        let cfg = Config::from_toml(include_str!("../../config.example.toml"))
            .expect("config.example.toml must parse");
        assert_eq!(cfg.server.port, 2333);
        assert_eq!(cfg.lavalink.server.password, "youshallnotpass");
        assert_eq!(cfg.lavalink.server.opus_bitrate, 96_000);
    }

    #[test]
    fn minimal_toml_uses_defaults() {
        let cfg = Config::from_toml("").expect("an empty config is all defaults");
        assert_eq!(cfg.server.port, 2333);
        assert_eq!(cfg.lavalink.server.player_update_interval, 5);
    }

    #[test]
    fn port_override_wins_over_the_file_value() {
        let mut cfg = Config::from_toml("[server]\nport = 2333\n").expect("valid server block");
        cfg.set_port(Some("2334"));
        assert_eq!(cfg.server.port, 2334);
    }

    #[test]
    fn absent_port_override_leaves_the_file_value() {
        let mut cfg = Config::from_toml("[server]\nport = 2334\n").expect("valid server block");
        cfg.set_port(None);
        assert_eq!(cfg.server.port, 2334);
    }

    #[test]
    #[should_panic(expected = "Invalid PORT 'nope'")]
    fn port_override_must_be_a_number() {
        Config::from_toml("")
            .expect("an empty config is all defaults")
            .set_port(Some("nope"));
    }
}
