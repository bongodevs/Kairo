//! Reading and layering the configuration document.

use super::*;

#[test]
fn empty_document_yields_defaults() {
    let config: Config = toml::from_str("").expect("empty toml");
    assert_eq!(config.server.address, "0.0.0.0");
    assert_eq!(config.server.port, 2333);
    assert!(!config.server.http2.enabled);
    assert_eq!(config.lavalink.server.player_update_interval, 5);
    assert_eq!(config.lavalink.server.track_stuck_threshold_ms, 10_000);
    assert_eq!(config.lavalink.server.session_timeout, 60);
    assert_eq!(config.lavalink.server.buffer_duration_ms, None);
    assert_eq!(config.lavalink.server.opus_encoding_quality, None);
    assert_eq!(config.lavalink.server.resampling_quality, None);
    assert_eq!(config.lavalink.server.opus_quality(), 10);
    assert!(config.lavalink.server.sources.http);
    assert!(!config.lavalink.server.sources.local);
    assert!(config.plugins.is_empty());
    assert!(config.lavalink.server.filters.is_empty());
    // A node with no crossfade block still hands tracks over gaplessly.
    assert!(!config.crossfade.enable);
    assert!(config.crossfade.gapless);
    assert_eq!(config.crossfade.duration_ms, 6000);
    assert_eq!(config.crossfade.manual_duration_ms, 3500);
    assert_eq!(config.crossfade.curve, CrossfadeCurve::SCurve);
    assert_eq!(config.logging.level, "info");
    assert_eq!(config.logging.format, LogFormat::Compact);
    assert!(config.logging.timestamps);
    assert!(config.logging.show_target);
    assert!(config.logging.color);
    assert!(config.logging.banner);
    assert!(config.logging.file.is_none());
    // Scraping is opt-in, as in the reference, at its documented default path.
    assert!(!config.metrics.prometheus.enabled);
    assert_eq!(config.metrics.prometheus.endpoint, "/metrics");
}

#[test]
fn committed_config_is_runnable() {
    let config: Config = toml::from_str(include_str!("../../config.toml")).expect("root config");
    assert_eq!(config.server.address, "0.0.0.0");
    assert_eq!(config.server.port, 2333);
    assert!(!config.server.http2.enabled);
    assert_eq!(config.lavalink.server.password.as_deref(), Some("youshallnotpass"));
    assert!(config.lavalink.server.sources.http);
    assert!(!config.lavalink.server.sources.local);
}

#[test]
fn logging_section_is_parsed() {
    let doc = r#"
[logging]
level = "debug"
format = "json"
timestamps = false
showTarget = true
color = false
banner = false

[logging.levels]
voice = "trace"
"reqwest::connect" = "debug"

[logging.file]
path = "logs"
maxFileSize = "10MB"
maxHistory = 7
"#;
    let config: Config = toml::from_str(doc).expect("logging config");
    assert_eq!(config.logging.level, "debug");
    assert_eq!(config.logging.format, LogFormat::Json);
    assert_eq!(config.logging.levels.get("voice").map(String::as_str), Some("trace"));
    assert_eq!(config.logging.levels.get("reqwest::connect").map(String::as_str), Some("debug"));
    assert!(!config.logging.timestamps);
    assert!(config.logging.show_target);
    assert!(!config.logging.color);
    assert!(!config.logging.banner);
    let file = config.logging.file.expect("file logging");
    assert_eq!(file.path, "logs");
    assert_eq!(file.max_file_size, LogSize(10 * 1024 * 1024));
    assert_eq!(file.max_history, 7);
}

#[test]
fn log_file_rotation_defaults_and_sizes_are_parsed() {
    let config: Config = toml::from_str("[logging.file]\npath = \"logs\"\n").expect("file");
    let file = config.logging.file.expect("file logging");
    assert_eq!(file.max_file_size, LogSize(1024 * 1024 * 1024));
    assert_eq!(file.max_history, 30);

    for (text, bytes) in [
        ("1024", 1024),
        ("2kb", 2 * 1024),
        ("3 MB", 3 * 1024 * 1024),
        ("1GB", 1024 * 1024 * 1024),
        ("0", 0),
    ] {
        let doc = format!("[logging.file]\npath = \"logs\"\nmaxFileSize = \"{text}\"\n");
        let parsed: Config = toml::from_str(&doc).expect("size");
        assert_eq!(parsed.logging.file.expect("file").max_file_size, LogSize(bytes), "{text}");
    }
    let numeric: Config =
        toml::from_str("[logging.file]\npath = \"logs\"\nmaxFileSize = 4096\n").expect("bytes");
    assert_eq!(numeric.logging.file.expect("file").max_file_size, LogSize(4096));

    let invalid: Result<Config, _> =
        toml::from_str("[logging.file]\npath = \"logs\"\nmaxFileSize = \"lots\"\n");
    assert!(invalid.is_err());
}

#[test]
fn network_timeouts_default_and_parse_like_the_reference() {
    let config: Config = toml::from_str("").expect("defaults");
    let timeouts = config.lavalink.server.timeouts;
    assert_eq!(timeouts.connect_timeout_ms, 3000);
    assert_eq!(timeouts.connection_request_timeout_ms, 3000);
    assert_eq!(timeouts.socket_timeout_ms, 3000);

    let doc = r#"
[lavalink.server.timeouts]
connectTimeoutMs = 1000
connectionRequestTimeoutMs = 2000
socketTimeoutMs = 3000
"#;
    let config: Config = toml::from_str(doc).expect("timeouts config");
    let timeouts = config.lavalink.server.timeouts;
    assert_eq!(timeouts.connect_timeout_ms, 1000);
    assert_eq!(timeouts.connection_request_timeout_ms, 2000);
    assert_eq!(timeouts.socket_timeout_ms, 3000);
}

#[test]
fn request_logging_defaults_and_overrides() {
    let config: Config = toml::from_str("").expect("defaults");
    let request = &config.logging.request;
    assert!(request.enabled);
    assert!(!request.include_client_info);
    assert!(!request.include_headers);
    assert!(request.include_query_string);
    assert!(request.include_payload);
    assert_eq!(request.max_payload_length, 10_000);
    assert!(!request.before_request);

    let doc = r#"
[logging.request]
enabled = false
includeClientInfo = true
includeHeaders = true
includeQueryString = false
includePayload = false
maxPayloadLength = 512
beforeRequest = true
"#;
    let config: Config = toml::from_str(doc).expect("request logging config");
    let request = &config.logging.request;
    assert!(!request.enabled);
    assert!(request.include_client_info);
    assert!(request.include_headers);
    assert!(!request.include_query_string);
    assert!(!request.include_payload);
    assert_eq!(request.max_payload_length, 512);
    assert!(request.before_request);
}

#[test]
fn metrics_prometheus_section_is_parsed() {
    let doc = "[metrics.prometheus]\nenabled = true\nendpoint = \"/prom\"\n";
    let config: Config = toml::from_str(doc).expect("metrics config");
    assert!(config.metrics.prometheus.enabled);
    assert_eq!(config.metrics.prometheus.endpoint, "/prom");
}

#[test]
fn crossfade_section_drives_the_engine_defaults() {
    let doc = r#"
[crossfade]
enable = true
durationMs = 4000
manualDurationMs = 2500
curve = "linear"
gapless = false
"#;
    let config: Config = toml::from_str(doc).expect("crossfade config");
    let engine = config.crossfade.to_engine().expect("enabled fade");
    assert_eq!(engine.duration_ms, 4000);
    assert_eq!(engine.manual_duration_ms, 2500);
    assert_eq!(engine.curve, player::effects::CrossfadeCurve::Linear);
    assert!(!engine.gapless);

    // Without a fade, the default still asks for a zero-length hand-off.
    let default = Config::default().crossfade.to_engine().expect("gapless");
    assert!(default.is_gapless());
    assert_eq!(default.manual_duration_ms, 3500);

    let off: Config = toml::from_str("[crossfade]\nenable = false\ngapless = false\n")
        .expect("all transitions off");
    assert!(off.crossfade.to_engine().is_none());
}

#[test]
fn camel_case_keys_are_parsed() {
    let doc = r#"
[server]
address = "127.0.0.1"
port = 9000

[server.http2]
enabled = true

[lavalink.server]
password = "hunter2"
playerUpdateInterval = 2
bufferDurationMs = 100
frameBufferDurationMs = 200
trackStuckThresholdMs = 250
useSeekGhosting = false
sessionTimeout = 30

[lavalink.server.sources]
http = false
local = true
youtube = true

[[plugin]]
name = "sources"

[plugin.config.jiosaavn]
secretKey = "12345678"
artistTopTracks = 12

[lavalink.server.filters]
volume = false
channelMix = true
high-pass = false

"#;
    let config: Config = toml::from_str(doc).expect("valid config");
    assert_eq!(config.server.port, 9000);
    assert!(config.server.http2.enabled);
    assert_eq!(config.lavalink.server.password.as_deref(), Some("hunter2"));
    assert_eq!(config.lavalink.server.buffer_duration_ms, Some(100));
    assert_eq!(config.lavalink.server.frame_buffer_duration_ms, 200);
    assert_eq!(config.lavalink.server.track_stuck_threshold_ms, 250);
    assert!(!config.lavalink.server.use_seek_ghosting);
    assert!(!config.lavalink.server.sources.http);
    assert!(config.lavalink.server.sources.local);
    assert_eq!(config.plugins[0].name, "sources");
    let plugin_debug = format!("{:?}", config.plugins[0]);
    assert!(plugin_debug.contains("<redacted>"));
    assert!(!plugin_debug.contains("12345678"));
    assert_eq!(config.lavalink.server.filters.get("volume"), Some(&false));
    assert_eq!(config.lavalink.server.filters.get("channelMix"), Some(&true));
    assert_eq!(config.lavalink.server.filters.get("high-pass"), Some(&false));
}

#[test]
fn source_tables_override_the_legacy_flags() {
    let doc = r#"
[lavalink.server.sources]
http = true
local = false

[sources.http]
enabled = false

[sources.local]
enabled = true

"#;
    let config: Config = toml::from_str(doc).expect("valid config");
    let flags = &config.lavalink.server.sources;
    let resolved = flags.source_config(&config.sources);
    assert!(!resolved.http);
    assert!(resolved.local);
}

#[test]
fn legacy_source_flags_still_apply_without_tables() {
    let doc = r#"
[lavalink.server.sources]
http = false
local = true
"#;
    let config: Config = toml::from_str(doc).expect("valid config");
    let flags = &config.lavalink.server.sources;
    let resolved = flags.source_config(&config.sources);
    assert!(!resolved.http);
    assert!(resolved.local);
}

#[test]
fn unknown_lavalink_key_is_not_an_error() {
    // later features add keys; old binaries should still start
    let config: Config =
        toml::from_str("[lavalink.server]\nsomeFutureKey = 1\n").expect("unknown keys tolerated");
    assert_eq!(config.lavalink.server.player_update_interval, 5);
    assert!(config.lavalink.server.use_seek_ghosting);
}

#[test]
fn missing_file_reports_not_found() {
    let err = Config::load(Path::new("/nonexistent/kairo-test-config.toml"))
        .expect_err("file does not exist");
    match err {
        ConfigError::Read { source, .. } => assert_eq!(source.kind(), io::ErrorKind::NotFound),
        other => panic!("expected read error, got {other}"),
    }
}

#[test]
fn broken_toml_reports_the_path() {
    let dir = std::env::temp_dir().join("kairo-config-test");
    fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("broken.toml");
    fs::write(&path, "server = [oops").expect("write");
    let err = Config::load(&path).expect_err("invalid toml");
    assert!(err.to_string().contains("broken.toml"), "{err}");
    fs::remove_file(&path).expect("cleanup");
}

#[test]
fn the_env_password_beats_whichever_base_the_process_starts_from() {
    let mut from_file = toml::from_str::<Config>("[lavalink.server]\npassword = \"from-file\"\n")
        .expect("valid config");
    from_file.override_password(Some("from-env"));
    assert_eq!(from_file.lavalink.server.password.as_deref(), Some("from-env"));

    let mut from_defaults = Config::default();
    from_defaults.override_password(Some("from-env"));
    assert_eq!(
        from_defaults.lavalink.server.password.as_deref(),
        Some("from-env"),
        "the no-file path is exactly the path S2b found broken"
    );
}

#[test]
fn an_absent_env_password_leaves_the_base_alone() {
    let mut config = toml::from_str::<Config>("[lavalink.server]\npassword = \"from-file\"\n")
        .expect("valid config");
    config.override_password(None);
    assert_eq!(config.lavalink.server.password.as_deref(), Some("from-file"));
}

#[test]
fn defaults_are_the_documented_numbers() {
    let config = Config::default();
    assert_eq!(config.server.address, "0.0.0.0");
    assert_eq!(config.server.port, 2333);
    assert_eq!(config.lavalink.server.session_timeout, 60);
    assert_eq!(config.lavalink.server.player_update_interval, 5);
}

#[test]
fn opus_and_resampling_quality_follow_the_reference_rules() {
    let config: Config = toml::from_str(
        r#"
[lavalink.server]
opusEncodingQuality = 6
resamplingQuality = "HIGH"
"#,
    )
    .expect("quality config");
    assert_eq!(config.lavalink.server.opus_quality(), 6);
    assert_eq!(config.lavalink.server.resampling_quality, Some(ResamplingQualityConfig::High));

    for value in ["-1", "11", "100"] {
        let config: Config =
            toml::from_str(&format!("[lavalink.server]\nopusEncodingQuality = {value}\n"))
                .expect("out-of-range config");
        assert_eq!(config.lavalink.server.opus_quality(), 10, "value {value}");
    }
}

#[test]
fn http_proxy_config_uses_reference_defaults_and_a_blank_host_opts_out() {
    let config: Config = toml::from_str(
        r#"
[lavalink.server.httpConfig]
proxyHost = "proxy.example"
proxyUser = "user"
proxyPassword = "secret"
"#,
    )
    .expect("proxy config");
    let block = config.lavalink.server.http_config.expect("proxy block");
    let proxy = block.to_proxy().expect("proxy");
    assert_eq!(proxy.url, "http://proxy.example:3128");
    assert_eq!(proxy.username.as_deref(), Some("user"));
    assert_eq!(proxy.password.as_deref(), Some("secret"));
    assert!(!format!("{block:?}").contains("secret"), "the proxy password must not be printed");

    let config: Config = toml::from_str(
        r#"
[lavalink.server.httpConfig]
proxyHost = "localhost"
proxyPort = 8080
"#,
    )
    .expect("proxy config");
    let proxy = config.lavalink.server.http_config.expect("proxy block").to_proxy();
    let proxy = proxy.expect("proxy");
    assert_eq!(proxy.url, "http://localhost:8080");
    assert!(proxy.username.is_none(), "blank proxyUser means no credentials");

    let config: Config =
        toml::from_str("[lavalink.server.httpConfig]\nproxyHost = \"  \"\n").expect("blank host");
    assert!(config.lavalink.server.http_config.expect("proxy block").to_proxy().is_none());
}

#[test]
fn ratelimit_section_matches_the_reference_defaults() {
    let config: Config = toml::from_str("").expect("defaults");
    assert!(config.lavalink.server.ratelimit.is_none());

    let config: Config = toml::from_str(
        r#"
[lavalink.server.ratelimit]
ipBlocks = ["10.0.0.0/24", "10.0.1.0/24"]
excludedIps = ["10.0.0.1"]
strategy = "LoadBalance"
retryLimit = 3
searchTriggersFail = false
"#,
    )
    .expect("ratelimit config");
    let block = config.lavalink.server.ratelimit.expect("ratelimit block");
    assert_eq!(block.ip_blocks, ["10.0.0.0/24", "10.0.1.0/24"]);
    assert_eq!(block.excluded_ips, ["10.0.0.1"]);
    assert_eq!(block.strategy, "LoadBalance");
    assert_eq!(block.retry_limit, 3);
    assert!(!block.search_triggers_fail);

    let defaults: Config =
        toml::from_str("[lavalink.server.ratelimit]\nipBlocks = [\"10.0.0.0/24\"]\n")
            .expect("defaults");
    let block = defaults.lavalink.server.ratelimit.expect("ratelimit block");
    assert_eq!(block.strategy, "RotateOnBan");
    assert_eq!(block.retry_limit, -1);
    assert!(block.search_triggers_fail);
}
