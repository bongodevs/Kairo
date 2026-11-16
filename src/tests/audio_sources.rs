use std::time::{Duration, Instant};

use player::source::{LoadOutcome, SourceReference};

use super::*;
use crate::config::ResamplingQualityConfig;
#[test]
fn default_setup_registers_only_http() {
    let (sources, _) = build(&Config::default(), None, &PluginManager::empty()).expect("sources");
    assert_eq!(sources.names().collect::<Vec<_>>(), ["http"]);
}

#[test]
fn both_direct_transports_can_be_disabled() {
    let mut config = Config::default();
    config.lavalink.server.sources.http = false;
    let (sources, _) = build(&config, None, &PluginManager::empty()).expect("sources");
    assert_eq!(sources.names().collect::<Vec<_>>(), Vec::<&str>::new());
}

#[test]
fn local_transport_registers_before_http() {
    let mut config = Config::default();
    config.lavalink.server.sources.local = true;
    let (sources, _) = build(&config, None, &PluginManager::empty()).expect("sources");
    assert_eq!(sources.names().collect::<Vec<_>>(), ["local", "http"]);
}

#[tokio::test]
async fn zero_player_update_interval_is_rejected_before_bind() {
    let mut config = Config::default();
    config.lavalink.server.player_update_interval = 0;
    let error = serve(&config).await.expect_err("zero interval must fail startup");
    assert!(error.to_string().contains("playerUpdateInterval"), "{error}");
}

#[tokio::test]
async fn the_configured_socket_timeout_reaches_the_http_source() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = [0u8; 2048];
        let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let mut config = Config::default();
    config.lavalink.server.timeouts.socket_timeout_ms = 50;
    let (sources, _) = build(&config, None, &PluginManager::empty()).expect("sources");

    let started = Instant::now();
    let outcome = sources.load(SourceReference::new(format!("http://{address}/track.wav"))).await;
    assert!(matches!(outcome, LoadOutcome::Error(_)), "a silent peer must fail the load");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the configured socket timeout was not applied"
    );
}

#[test]
fn opus_and_resampling_settings_reach_the_engine_config() {
    let mut config = Config::default();
    config.lavalink.server.opus_encoding_quality = Some(3);
    config.lavalink.server.resampling_quality = Some(ResamplingQualityConfig::High);

    let player = player_config(&config).expect("player config");
    assert_eq!(player.opus.complexity, 3);
    assert_eq!(player.resampling_quality, player::pcm::ResamplingQuality::High);

    config.lavalink.server.opus_encoding_quality = Some(99);
    let player = player_config(&config).expect("player config");
    assert_eq!(player.opus.complexity, 10, "out-of-range quality falls back to the maximum");
}

#[tokio::test]
async fn the_configured_http_proxy_carries_generic_http_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let proxy_address = listener.local_addr().expect("proxy address");
    let wave = crate::test_support::test_wave();
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = [0u8; 4096];
        let length =
            tokio::io::AsyncReadExt::read(&mut socket, &mut request).await.expect("read request");
        let request = String::from_utf8_lossy(&request[..length]).into_owned();
        assert!(
            request.starts_with("GET http://192.0.2.1:9/track.wav HTTP/1.1"),
            "the request must go through the proxy: {request}"
        );
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wave.len()
        );
        tokio::io::AsyncWriteExt::write_all(&mut socket, head.as_bytes()).await.expect("head");
        tokio::io::AsyncWriteExt::write_all(&mut socket, &wave).await.expect("body");
        request
    });

    let mut config = Config::default();
    config.lavalink.server.http_config = Some(crate::config::HttpProxyConfig {
        proxy_host: proxy_address.ip().to_string(),
        proxy_port: proxy_address.port(),
        proxy_user: String::new(),
        proxy_password: String::new(),
    });
    let (sources, _) = build(&config, None, &PluginManager::empty()).expect("sources");
    let outcome = sources.load(SourceReference::new("http://192.0.2.1:9/track.wav")).await;
    assert!(matches!(outcome, LoadOutcome::Track(_)), "proxy load failed: {outcome:?}");
    let request = proxy.await.expect("proxy task");
    assert!(request.to_ascii_lowercase().contains("host: 192.0.2.1:9"), "{request}");
}

#[test]
fn an_unused_buffer_duration_is_reported_once() {
    let capture = crate::test_support::LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(crate::test_support::capture_subscriber(&capture));

    let mut config = Config::default();
    config.lavalink.server.buffer_duration_ms = Some(400);
    let _ = player_config(&config).expect("player config");
    let text = capture.text();
    assert!(text.contains("bufferDurationMs"), "{text}");
    assert!(text.contains("no equivalent queue"), "{text}");

    let capture = crate::test_support::LogCapture::default();
    drop(_guard);
    let _guard =
        tracing::subscriber::set_default(crate::test_support::capture_subscriber(&capture));
    let mut config = Config::default();
    config.lavalink.server.buffer_duration_ms = None;
    let _ = player_config(&config).expect("player config");
    assert!(!capture.text().contains("bufferDurationMs"), "{}", capture.text());
}

#[tokio::test]
async fn an_enabled_route_planner_binds_generic_http_requests() {
    use std::net::IpAddr;

    use crate::config::RateLimitConfig;
    use crate::route_planner::RoutePlanner;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let wave = crate::test_support::test_wave();
    let server = tokio::spawn(async move {
        let (mut socket, peer) = listener.accept().await.expect("accept");
        let mut request = [0u8; 2048];
        let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wave.len()
        );
        tokio::io::AsyncWriteExt::write_all(&mut socket, head.as_bytes()).await.expect("head");
        tokio::io::AsyncWriteExt::write_all(&mut socket, &wave).await.expect("body");
        peer.ip()
    });

    let mut config = Config::default();
    config.lavalink.server.ratelimit = Some(RateLimitConfig {
        ip_blocks: vec![String::from("127.0.0.0/8")],
        excluded_ips: vec![String::from("127.0.0.0"), String::from("127.0.0.1")],
        strategy: String::from("RotateOnBan"),
        retry_limit: -1,
        search_triggers_fail: true,
    });
    let planner = RoutePlanner::build(config.lavalink.server.ratelimit.as_ref())
        .expect("config")
        .expect("planner");
    let (sources, _) = build(&config, Some(&std::sync::Arc::new(planner)), &PluginManager::empty())
        .expect("sources");
    let outcome = sources.load(SourceReference::new(format!("http://{address}/track.wav"))).await;
    assert!(matches!(outcome, LoadOutcome::Track(_)), "planner load failed: {outcome:?}");
    assert_eq!(server.await.expect("server"), IpAddr::from([127, 0, 0, 2]));
}
