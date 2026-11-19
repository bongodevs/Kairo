//! Exposition rendering and the routed endpoint: unit-level for the text builder, router-level for
//! the enable flag, the auth exemption and the `name[]` filter.

use tokio::io::AsyncReadExt;

use super::collector::{Cpu, Memory};
use super::*;
fn stats() -> Stats {
    Stats {
        players: 3,
        playing_players: 1,
        uptime: 4321,
        memory: Memory { free: 2048, used: 1024, allocated: 4096, reservable: 4096 },
        cpu: Cpu { cores: 4, system_load: 0.125, lavalink_load: 0.5 },
    }
}

#[test]
fn exposition_carries_the_reference_family_shapes() {
    let body = render(&families(&stats(), &SearchMetrics::default()));
    for line in [
        "# TYPE lavalink_players_total gauge",
        "lavalink_players_total 3",
        "# TYPE lavalink_playing_players_total gauge",
        "lavalink_playing_players_total 1",
        "lavalink_uptime_milliseconds 4321",
        "lavalink_memory_free_bytes 2048",
        "lavalink_memory_used_bytes 1024",
        "lavalink_memory_allocated_bytes 4096",
        "lavalink_memory_reservable_bytes 4096",
        "lavalink_cpu_cores 4",
        // the reference publishes the 0..=1 fractions unchanged despite the name
        "lavalink_cpu_system_load_percentage 0.125",
        "lavalink_cpu_lavalink_load_percentage 0.5",
        "# TYPE logback_events_total counter",
    ] {
        assert!(body.contains(line), "missing {line:?} in:\n{body}");
    }
    for level in LEVEL_NAMES {
        assert!(
            body.contains(&format!("logback_events_total{{level=\"{level}\"}}")),
            "missing the {level} sample"
        );
    }
    // Every family has its HELP line, and the text ends in a newline like the Java writer.
    assert_eq!(body.matches("# HELP ").count(), body.matches("# TYPE ").count());
    assert!(body.ends_with('\n'));
}

#[test]
fn name_parameters_select_families() {
    assert_eq!(requested(""), Vec::<String>::new());
    assert_eq!(requested("foo=bar"), Vec::<String>::new());
    assert_eq!(
        requested("name[]=lavalink_players_total&name[]=lavalink_cpu_cores"),
        ["lavalink_players_total", "lavalink_cpu_cores"]
    );
    // The encoded form is what many scrapers actually send.
    assert_eq!(
        requested("name%5B%5D=lavalink_uptime_milliseconds"),
        ["lavalink_uptime_milliseconds"]
    );
}

#[test]
fn percent_decoding_survives_noise() {
    assert_eq!(percent_decode("a%20b"), "a b");
    assert_eq!(percent_decode("a+b"), "a b");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
    assert_eq!(percent_decode("plain"), "plain");
}

mod http {
    use std::net::{IpAddr, SocketAddr};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::test_support::app_state_with_sources;
    use player::source::Sources;

    /// The real router, on an ephemeral port, with a password set and the endpoint on or off.
    pub(crate) async fn spawn(prometheus: bool) -> SocketAddr {
        spawn_with_sources(prometheus, Sources::new()).await
    }

    pub(crate) async fn spawn_with_sources(prometheus: bool, sources: Sources) -> SocketAddr {
        let mut state = app_state_with_sources(Some("letmein"), sources);
        state.metrics_endpoint = prometheus.then(|| "/metrics".to_owned());
        let app = crate::app::router(state);
        let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .await;
        });
        addr
    }

    pub(crate) async fn get_authorized(addr: SocketAddr, path: &str) -> (String, String) {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: letmein\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let split = text.find("\r\n\r\n").unwrap();
        (text[..split].to_owned(), text[split + 4..].to_owned())
    }

    /// A one-shot HTTP/1.1 GET, returned as (head, body).
    pub(crate) async fn get(addr: SocketAddr, path: &str) -> (String, String) {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let split = text.find("\r\n\r\n").unwrap();
        (text[..split].to_owned(), text[split + 4..].to_owned())
    }
}

#[tokio::test]
async fn an_enabled_endpoint_is_auth_exempt_prometheus_text() {
    let addr = http::spawn(true).await;
    // A scraper sends no password, and the reference exempts this path; so do we.
    let (head, body) = http::get(addr, "/metrics").await;
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/plain; version=0.0.4; charset=utf-8"),
        "{head}"
    );
    assert!(body.contains("lavalink_players_total 0"), "{body}");
    assert!(body.contains("lavalink_cpu_cores 1"), "{body}");
    assert!(body.contains("# TYPE logback_events_total counter"), "{body}");
}

#[tokio::test]
async fn a_disabled_endpoint_is_not_exempt_and_not_served() {
    let addr = http::spawn(false).await;
    // With the controller absent the reference exempts nothing: the path is an ordinary request,
    // so it meets the password filter before anything else.
    let (head, _) = http::get(addr, "/metrics").await;
    assert!(
        head.starts_with("HTTP/1.1 401"),
        "a disabled endpoint keeps no anonymous route: {head}"
    );
    // Credentialed, there is simply nothing there.
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(
        &mut stream,
        b"GET /metrics HTTP/1.1\r\nHost: x\r\nAuthorization: letmein\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    assert!(raw.starts_with("HTTP/1.1 404"), "{raw}");
}

#[tokio::test]
async fn a_name_parameter_writes_exactly_one_family() {
    let addr = http::spawn(true).await;
    let (_, body) = http::get(addr, "/metrics?name[]=lavalink_cpu_cores").await;
    assert!(body.contains("lavalink_cpu_cores 1"), "{body}");
    assert_eq!(body.matches("# TYPE ").count(), 1, "{body}");
    assert!(!body.contains("lavalink_uptime_milliseconds"), "{body}");
}

#[test]
fn search_counters_render_the_reference_families() {
    let metrics = SearchMetrics::default();
    metrics.record_load("local", "track");
    metrics.record_load("local", "track");
    metrics.record_load("http", "error");
    metrics.record_play("local", "42");

    let body = render(&families(&stats(), &metrics));
    assert!(body.contains("# TYPE lavalink_tracks_played_total counter"), "{body}");
    assert!(
        body.contains(r#"lavalink_tracks_played_total{source="local",guild_id="42"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"lavalink_item_load_results_total{source="local",result="track"} 2"#),
        "{body}"
    );
    assert!(
        body.contains(r#"lavalink_item_load_results_total{source="http",result="error"} 1"#),
        "{body}"
    );

    let empty = render(&families(&stats(), &SearchMetrics::default()));
    assert!(empty.contains("# TYPE lavalink_item_load_results_total counter"), "{empty}");
    assert!(!empty.contains("lavalink_item_load_results_total{"), "{empty}");
}

#[test]
fn search_metrics_rejects_invalid_guild_id_and_formats_u64() {
    let metrics = SearchMetrics::default();
    metrics.record_play("local", "not-a-number");
    assert!(metrics.play_samples().is_empty());

    metrics.record_play("local", "123456789012345678");
    let samples = metrics.play_samples();
    assert_eq!(samples.len(), 1);
    assert_eq!(
        samples[0],
        r#"lavalink_tracks_played_total{source="local",guild_id="123456789012345678"} 1"#
    );
}

#[tokio::test]
async fn a_load_records_the_dev_counter() {
    let addr = http::spawn_with_sources(true, crate::test_support::memory_sources()).await;
    let (head, _) = http::get_authorized(addr, "/v4/loadtracks?identifier=memory.wav").await;
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");

    let (_, body) = http::get(addr, "/metrics").await;
    assert!(
        body.contains(r#"lavalink_item_load_results_total{source="local",result="track"} 1"#),
        "{body}"
    );
    let (_, selected) = http::get(addr, "/metrics?name[]=lavalink_item_load_results_total").await;
    assert!(selected.contains("result=\"track\""), "{selected}");
    assert_eq!(selected.matches("# TYPE ").count(), 1, "{selected}");
}
