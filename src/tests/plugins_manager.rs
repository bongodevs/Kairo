use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kairo_plugin_api::Source;
use player::source::{LoadOutcome, SourceConfig, Sources};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::*;
use crate::plugins::load::validate_manifest;

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn root(name: &str) -> std::path::PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kairo-{name}-{}-{sequence}", std::process::id()))
}

fn config(name: &str) -> PluginConfig {
    PluginConfig { name: name.into(), repo: None, tag: None, config: toml::Table::new() }
}

#[tokio::test]
async fn empty_configuration_does_not_discover_files_or_create_directories() {
    let root = root("plugin-empty");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    tokio::fs::write(root.join("unconfigured.kai"), b"not a plugin")
        .await
        .expect("unconfigured file");
    let manager = PluginManager::load(&[], &root).await.expect("empty manager");

    assert_eq!(manager.counts(), (0, 0));
    assert!(!root.join(".run").exists());
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn missing_configured_plugin_becomes_failed_state() {
    let root = root("plugin-missing");
    let manager = PluginManager::load(&[config("missing")], &root).await.expect("manager");

    assert_eq!(manager.counts(), (0, 1));
    assert!(manager.failure("missing").unwrap().contains("copy plugin"));
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn a_library_for_another_os_is_rejected_before_loading() {
    let root = root("plugin-platform");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    let config = config("hello-source");
    let foreign: &[u8] =
        if cfg!(target_os = "windows") { b"\x7fELF\x02\x01\x01\x00" } else { b"MZ\x90\x00" };
    tokio::fs::write(config.install_path(&root), foreign).await.expect("foreign library");

    let manager = PluginManager::load(&[config], &root).await.expect("manager");
    let reason = manager.failure("hello-source").expect("failure");
    assert!(reason.contains("hello-source"), "{reason}");
    assert!(reason.contains("built for"), "{reason}");
    assert_eq!(manager.counts(), (0, 1));
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn reload_reports_unknown_and_unregistered_plugins() {
    let empty = PluginManager::empty();
    assert!(matches!(empty.reload("nope").await, Err(ReloadError::Unknown { .. })));

    let root = root("plugin-reload-unavailable");
    let manager = PluginManager::load(&[config("missing")], &root).await.expect("manager");
    assert!(matches!(manager.reload("missing").await, Err(ReloadError::Unavailable { .. })));
    manager.shutdown().await;
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn a_quarantined_build_is_not_loaded_and_keeps_its_marker() {
    let root = root("plugin-quarantine");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    let config = config("sources");
    tokio::fs::write(config.install_path(&root), b"crashing build")
        .await
        .expect("installed plugin");
    let staged = stage::stage(&config, &root).await.expect("stage");
    guard::arm(&root, "sources", &staged.hash).await.expect("guard");

    let manager = PluginManager::load(&[config], &root).await.expect("manager");
    assert_eq!(manager.counts(), (0, 1));
    let listing = manager.listing();
    assert_eq!(listing[0].state, "quarantined");
    assert!(listing[0].reason.as_deref().unwrap_or_default().contains("quarantined"));
    assert!(matches!(manager.reload("sources").await, Err(ReloadError::Quarantined { .. })));

    manager.shutdown().await;
    assert!(root.join(".guard/sources").exists(), "a quarantined marker survives a graceful stop");
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[test]
fn manifest_validation_checks_identity_version_and_source_names() {
    let source = Source::new("demo");
    let valid = Manifest::new("plugin", "1.0.0").source(source.clone());
    validate_manifest("plugin", &valid).expect("valid manifest");

    let wrong_name = Manifest::new("other", "1.0.0").source(source.clone());
    assert!(validate_manifest("plugin", &wrong_name).unwrap_err().contains("does not match"));
    let empty_version = Manifest::new("plugin", " ").source(source.clone());
    assert!(validate_manifest("plugin", &empty_version).unwrap_err().contains("version"));
    let invalid_source = Manifest::new("plugin", "1.0.0").source(Source::new("Bad_Name"));
    assert!(validate_manifest("plugin", &invalid_source).unwrap_err().contains("invalid"));
    let duplicate = Manifest::new("plugin", "1.0.0").source(source.clone()).source(source);
    assert!(validate_manifest("plugin", &duplicate).unwrap_err().contains("duplicated"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "run after `cargo build` in crates/sources"]
async fn real_fixture_is_staged_loaded_and_reported() {
    let root = root("plugin-manager");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind media");
    let address = listener.local_addr().expect("media address");
    let preview_url = format!("http://{address}/preview.wav");
    let server = tokio::spawn(async move {
        let mut targets = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("request");
            let mut request = vec![0; 4096];
            let read = stream.read(&mut request).await.expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("request target")
                .to_owned();
            targets.push(target.clone());
            let (content_type, body) = if target == "/preview.wav" {
                ("audio/ogg", b"audio".to_vec())
            } else {
                let body = format!(
                    r#"{{"results":[{{"id":"song-id","song":"World","primary_artists":"Artist","duration":30,"media_preview_url":"{preview_url}"}}]}}"#
                );
                ("application/json", body.into_bytes())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.expect("write head");
            stream.write_all(&body).await.expect("write body");
        }
        targets
    });
    let mut config = config("sources");
    let mut jiosaavn = toml::Table::new();
    jiosaavn.insert(String::from("secretKey"), toml::Value::String(String::from("12345678")));
    jiosaavn
        .insert(String::from("endpoint"), toml::Value::String(format!("http://{address}/api.php")));
    jiosaavn.insert(String::from("mirror"), toml::Value::Boolean(false));
    config.config.insert(String::from("jiosaavn"), toml::Value::Table(jiosaavn));
    let fixture = crate::test_support::sources_plugin();
    tokio::fs::copy(fixture, config.install_path(&root)).await.expect("install fixture");

    let manager = PluginManager::load(&[config], &root).await.expect("manager");
    assert_eq!(manager.counts(), (1, 0));
    assert_eq!(manager.source_names(), ["jiosaavn"]);
    let mut providers = ProviderManager::new();
    let mirror = manager
        .register_sources(&mut providers, ["http", "local"], Vec::new())
        .expect("register plugin");
    let sources = Arc::new(
        providers.build(SourceConfig { http: false, local: false }).expect("source engine"),
    );
    mirror.install(&sources);
    let LoadOutcome::Search(tracks) = sources.search("jsprev:jssearch:world").await else {
        panic!("expected plugin search");
    };
    let track = tracks.first().expect("search track");
    assert_eq!(track.info().map(|info| info.source_name.as_str()), Some("jiosaavn"));
    let mut opened = sources.open_track(track).await.expect("open plugin track").expect("media");
    assert_eq!(opened.source_name(), "jiosaavn");
    assert_eq!(opened.reference().identifier(), "song-id");
    assert_eq!(opened.hints().mime_type.as_deref(), Some("audio/ogg"));
    assert!(opened.is_seekable());
    let mut body = [0; 5];
    opened.read(&mut body).await.expect("read media");
    assert_eq!(&body, b"audio");
    drop(opened);
    let targets = server.await.expect("media server");
    assert!(targets[0].contains("search.getResults"));
    assert_eq!(targets[1], "/preview.wav");
    drop(sources);
    manager.shutdown().await;
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

struct SearchServer {
    address: SocketAddr,
    received: mpsc::Receiver<()>,
    task: tokio::task::JoinHandle<()>,
}

async fn search_server(calls: usize, delay: Duration) -> SearchServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind search");
    let address = listener.local_addr().expect("search address");
    let (sent, received) = mpsc::channel(calls.max(1));
    let task = tokio::spawn(async move {
        let mut handlers = JoinSet::new();
        for _ in 0..calls {
            let (mut stream, _) = listener.accept().await.expect("accept search");
            let sent = sent.clone();
            handlers.spawn(async move {
                let mut request = vec![0; 8192];
                let read = stream.read(&mut request).await.expect("read search request");
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(
                    request.contains("search.getResults"),
                    "unexpected search request: {request}"
                );
                let _ = sent.send(()).await;
                tokio::time::sleep(delay).await;
                let body = r#"{"results":[{"id":"song-id","song":"World","primary_artists":"Artist","duration":30}]}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.expect("write search head");
                stream.write_all(body.as_bytes()).await.expect("write search body");
            });
        }
        while let Some(handler) = handlers.join_next().await {
            handler.expect("search handler");
        }
    });
    SearchServer { address, received, task }
}

fn search_config(name: &str, address: SocketAddr) -> PluginConfig {
    let mut config = config(name);
    let mut jiosaavn = toml::Table::new();
    jiosaavn.insert(String::from("secretKey"), toml::Value::String(String::from("12345678")));
    jiosaavn
        .insert(String::from("endpoint"), toml::Value::String(format!("http://{address}/api.php")));
    jiosaavn.insert(String::from("mirror"), toml::Value::Boolean(false));
    config.config.insert(String::from("jiosaavn"), toml::Value::Table(jiosaavn));
    config
}

async fn install_fixture(config: &PluginConfig, root: &std::path::Path) {
    tokio::fs::copy(crate::test_support::sources_plugin(), config.install_path(root))
        .await
        .expect("install fixture");
}

async fn loaded_sources(
    root: &std::path::Path,
    config: PluginConfig,
) -> (PluginManager, Arc<Sources>) {
    let manager = PluginManager::load(&[config], root).await.expect("manager");
    let mut providers = ProviderManager::new();
    let mirror = manager
        .register_sources(&mut providers, ["http", "local"], Vec::new())
        .expect("register plugin");
    let sources = Arc::new(
        providers.build(SourceConfig { http: false, local: false }).expect("source engine"),
    );
    mirror.install(&sources);
    (manager, sources)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run after `cargo build` in crates/sources"]
async fn real_fixture_handles_concurrent_search_http_load() {
    const CALLS: usize = 32;

    let root = root("plugin-search-load");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    let server = search_server(CALLS, Duration::ZERO).await;
    let config = search_config("sources", server.address);
    install_fixture(&config, &root).await;
    let (manager, sources) = loaded_sources(&root, config).await;
    drop(server.received);

    let mut calls = JoinSet::new();
    for _ in 0..CALLS {
        let sources = Arc::clone(&sources);
        calls.spawn(async move { sources.search("jssearch:world").await });
    }
    let mut completed = 0;
    while let Some(outcome) = calls.join_next().await {
        let LoadOutcome::Search(tracks) = outcome.expect("search task") else {
            panic!("expected plugin search results");
        };
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].info().map(|info| info.title.as_str()), Some("World"));
        completed += 1;
    }
    assert_eq!(completed, CALLS);
    server.task.await.expect("search server");

    drop(sources);
    manager.shutdown().await;
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run after `cargo build` in crates/sources"]
async fn shutdown_waits_for_in_flight_search_calls() {
    const CALLS: usize = 8;

    let root = root("plugin-shutdown-load");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    let mut server = search_server(CALLS, Duration::from_millis(150)).await;
    let config = search_config("sources", server.address);
    install_fixture(&config, &root).await;
    let (manager, sources) = loaded_sources(&root, config).await;

    let mut calls = JoinSet::new();
    for _ in 0..CALLS {
        let sources = Arc::clone(&sources);
        calls.spawn(async move { sources.search("jssearch:world").await });
    }
    for _ in 0..CALLS {
        server.received.recv().await.expect("search request reached the plugin");
    }

    let shutdown = tokio::spawn(async move { manager.shutdown().await });
    while let Some(outcome) = calls.join_next().await {
        let LoadOutcome::Search(tracks) = outcome.expect("search task") else {
            panic!("in-flight search did not survive shutdown");
        };
        assert_eq!(tracks.len(), 1);
    }
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("shutdown deadlocked behind in-flight calls")
        .expect("shutdown task");
    server.task.await.expect("search server");

    drop(sources);
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run after building the sources plugin"]
async fn real_fixture_reload_keeps_serving_new_calls() {
    let root = root("plugin-reload-fixture");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    let server = search_server(2, Duration::ZERO).await;
    let config = search_config("sources", server.address);
    install_fixture(&config, &root).await;
    let (manager, sources) = loaded_sources(&root, config).await;

    let LoadOutcome::Search(tracks) = sources.search("jssearch:world").await else {
        panic!("search before reload");
    };
    assert_eq!(tracks.len(), 1);

    let entry = manager.reload("sources").await.expect("reload");
    assert_eq!(entry.state, "loaded");
    assert_eq!(entry.version.as_deref(), Some("0.0.1-beta"));
    assert_eq!(entry.sources, ["jiosaavn"]);

    let LoadOutcome::Search(tracks) = sources.search("jssearch:world").await else {
        panic!("search after reload");
    };
    assert_eq!(tracks.len(), 1);
    server.task.await.expect("search server");

    drop(sources);
    manager.shutdown().await;
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run after `cargo build` in crates/sources"]
async fn reload_cycles_do_not_hang_or_lose_state() {
    for cycle in 0..3 {
        let root = root(&format!("plugin-reload-{cycle}"));
        tokio::fs::create_dir_all(&root).await.expect("plugin root");
        let server = search_server(1, Duration::ZERO).await;
        let config = search_config("sources", server.address);
        install_fixture(&config, &root).await;
        let (manager, sources) = loaded_sources(&root, config).await;
        drop(server.received);

        let LoadOutcome::Search(tracks) = sources.search("jssearch:world").await else {
            panic!("reload cycle {cycle} did not return search results");
        };
        assert_eq!(tracks.len(), 1);
        server.task.await.expect("search server");

        drop(sources);
        manager.shutdown().await;
        tokio::fs::remove_dir_all(root).await.expect("cleanup");
    }
}
