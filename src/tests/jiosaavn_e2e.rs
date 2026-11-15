use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use des::Des;
use des::cipher::generic_array::GenericArray;
use des::cipher::{BlockEncrypt, KeyInit};
use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use player::audio::MAX_PACKET_BYTES;
use player::buffer::{BufferRead, FrameBufferConfig};
use player::playback::{PlayerConfig, spawn_player};
use player::source::{LoadOutcome, ProviderManager, SourceConfig, SourceReference};
use player::track::codec::DecodeTrack;

use crate::config::{FallbackPlaceholder, FallbackProvider, PluginConfig};
use crate::plugins::PluginManager;
use crate::test_support::{app_state_with_shared_sources, request, spawn_server, test_wave};

#[tokio::test]
#[ignore = "run after `cargo build` in crates/sources"]
async fn rest_load_decode_and_player_playback_preserve_jiosaavn_track_state() {
    let (endpoint, service) = mock_service().await;
    let root = plugin_root("jiosaavn-e2e");
    let plugin = plugin_config(&endpoint, true);
    install_fixture(&plugin, &root).await;
    let manager = PluginManager::load(&[plugin], &root).await.expect("plugin manager");
    let mut providers = ProviderManager::new().with_probe(player::source_probe::audio_probe());
    let mirror = manager
        .register_sources(&mut providers, ["http", "local"], Vec::new())
        .expect("register plugin");
    let sources =
        Arc::new(providers.build(SourceConfig { http: false, local: false }).expect("sources"));
    mirror.install(&sources);
    let state = app_state_with_shared_sources(Some("test"), Arc::clone(&sources));
    let playback_sources = state.sources().clone();
    let address = spawn_server(state).await;

    let path = concat!(
        "/v4/loadtracks?identifier=https%3A%2F%2Fwww.jiosaavn.com",
        "%2Fsong%2Fdemo%2Fsong-id"
    );
    let loaded = request(address, "GET", path, Some("test"), None).await;
    assert_eq!(loaded.status, 200);
    let loaded: serde_json::Value = serde_json::from_str(&loaded.body).expect("load JSON");
    assert_eq!(loaded["loadType"], "track");
    assert_eq!(loaded["data"]["info"]["sourceName"], "jiosaavn");
    assert_eq!(loaded["data"]["pluginInfo"]["isPreview"], false);
    assert_eq!(loaded["data"]["pluginInfo"]["albumName"], "Demo Album");
    let encoded = loaded["data"]["encoded"].as_str().expect("encoded").to_owned();

    let request_body = serde_json::to_string(&vec![encoded.clone()]).expect("request JSON");
    let decoded =
        request(address, "POST", "/v4/decodetracks", Some("test"), Some(&request_body)).await;
    assert_eq!(decoded.status, 200);
    let decoded_json: serde_json::Value = serde_json::from_str(&decoded.body).expect("decode JSON");
    assert_eq!(decoded_json[0]["pluginInfo"], loaded["data"]["pluginInfo"]);

    let DecodeTrack::Track(track) = playback_sources.decode_track(&encoded).expect("decode track")
    else {
        panic!("expected track");
    };
    let config = PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer"),
        ..PlayerConfig::default()
    };
    let (player, mut frames, _events, guard) =
        spawn_player(playback_sources, config).expect("player");
    player.play(track, false).await.expect("play");
    let mut packet = [0_u8; MAX_PACKET_BYTES];
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match frames.read(&mut packet).expect("frame read") {
                BufferRead::Frame(_) => break,
                BufferRead::Empty => tokio::task::yield_now().await,
                BufferRead::End | BufferRead::Closed => panic!("playback ended before audio"),
            }
        }
    })
    .await
    .expect("audio timeout");
    guard.shutdown().await.expect("shutdown");
    let targets = service.await.expect("service");
    assert!(targets[0].contains("type=song"));
    assert!(targets[1].contains("song.getDetails"));
    assert_eq!(targets[2], "/media.wav");
    manager.shutdown().await;
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
#[ignore = "run after `cargo build` in crates/sources"]
async fn mirror_playback_uses_the_first_configured_fallback() {
    let (endpoint, template, service) = mock_mirror_service().await;
    let root = plugin_root("jiosaavn-mirror");
    let plugin = plugin_config(&endpoint, true);
    install_fixture(&plugin, &root).await;
    let manager = PluginManager::load(&[plugin], &root).await.expect("plugin manager");
    let mut providers = ProviderManager::new();
    let mirror = manager
        .register_sources(
            &mut providers,
            ["http", "local"],
            vec![FallbackProvider {
                source: String::from("http"),
                template,
                placeholder: FallbackPlaceholder::Query,
            }],
        )
        .expect("register plugin");
    let sources =
        Arc::new(providers.build(SourceConfig { http: true, local: false }).expect("sources"));
    mirror.install(&sources);

    let LoadOutcome::Track(track) =
        sources.load(SourceReference::new("https://www.jiosaavn.com/song/demo/song-id")).await
    else {
        panic!("expected plugin track");
    };
    let opened = sources.open_track(&track).await.expect("open mirror").expect("mirror");
    assert_eq!(opened.source_name(), "jiosaavn");
    assert_eq!(opened.reference().identifier(), "song-id");
    let mut riff = [0; 4];
    opened.into_stream().read_exact(&mut riff).await.expect("read mirror");
    assert_eq!(&riff, b"RIFF");

    let targets = service.await.expect("service");
    assert!(targets[0].contains("webapi.get"));
    assert!(targets[1].starts_with("/mirror.wav?q=Song"), "{}", targets[1]);
    drop(sources);
    manager.shutdown().await;
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

fn plugin_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kairo-{name}-{}", std::process::id()))
}

fn plugin_config(endpoint: &Url, mirror: bool) -> PluginConfig {
    let mut jiosaavn = toml::Table::new();
    jiosaavn.insert(String::from("secretKey"), toml::Value::String(String::from("12345678")));
    jiosaavn.insert(String::from("endpoint"), toml::Value::String(endpoint.to_string()));
    jiosaavn.insert(String::from("mirror"), toml::Value::Boolean(mirror));
    let mut config = toml::Table::new();
    config.insert(String::from("jiosaavn"), toml::Value::Table(jiosaavn));
    PluginConfig { name: String::from("sources"), repo: None, tag: None, config }
}

async fn install_fixture(config: &PluginConfig, root: &std::path::Path) {
    tokio::fs::create_dir_all(root).await.expect("plugin root");
    tokio::fs::copy(crate::test_support::sources_plugin(), config.install_path(root))
        .await
        .expect("install fixture");
}

async fn mock_service() -> (Url, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await.expect("listener");
    let address = listener.local_addr().expect("address");
    let media_url = format!("http://{address}/media.wav");
    let encrypted_url = encrypted_url(&media_url, b"12345678");
    let service = tokio::spawn(async move {
        let mut targets = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.expect("request");
            let mut request = vec![0_u8; 8 * 1024];
            let count = stream.read(&mut request).await.expect("read request");
            let request = String::from_utf8_lossy(&request[..count]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("target")
                .to_owned();
            targets.push(target.clone());
            let (content_type, body) = if target == "/media.wav" {
                ("audio/wav", test_wave())
            } else if target.contains("song.getDetails") {
                let body = format!(
                    r#"{{"song-id":{{"encrypted_media_url":"{encrypted_url}","320kbps":false}}}}"#
                );
                ("application/json", body.into_bytes())
            } else {
                let body = r#"{"songs":[{"id":"song-id","title":"Song","image":"https://img/150x150.jpg","perma_url":"https://www.jiosaavn.com/song/demo/song-id","more_info":{"duration":"30","album":"Demo Album","album_url":"https://www.jiosaavn.com/album/demo/album-id","artistMap":{"primary_artists":[{"name":"Artist"}]}}}]}"#.to_owned();
                ("application/json", body.into_bytes())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.expect("head");
            stream.write_all(&body).await.expect("body");
        }
        targets
    });
    let endpoint = Url::parse(&format!("http://{address}/api.php")).expect("endpoint");
    (endpoint, service)
}

async fn mock_mirror_service() -> (Url, String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await.expect("listener");
    let address = listener.local_addr().expect("address");
    let service = tokio::spawn(async move {
        let mut targets = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("request");
            let mut request = vec![0_u8; 8 * 1024];
            let count = stream.read(&mut request).await.expect("read request");
            let request = String::from_utf8_lossy(&request[..count]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("target")
                .to_owned();
            targets.push(target.clone());
            let (content_type, body) = if target.starts_with("/mirror.wav") {
                ("audio/wav", test_wave())
            } else {
                let body = br#"{"songs":[{"id":"song-id","title":"Song","perma_url":"https://www.jiosaavn.com/song/demo/song-id","more_info":{"duration":"30","artistMap":{"primary_artists":[{"name":"Artist"}]}}}]}"#.to_vec();
                ("application/json", body)
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.expect("head");
            stream.write_all(&body).await.expect("body");
        }
        targets
    });
    let endpoint = Url::parse(&format!("http://{address}/api.php")).expect("endpoint");
    let template = format!("http://{address}/mirror.wav?q=%QUERY%");
    (endpoint, template, service)
}

fn encrypted_url(url: &str, key: &[u8; 8]) -> String {
    let mut bytes = url.as_bytes().to_vec();
    let padding = 8 - bytes.len() % 8;
    bytes.extend(std::iter::repeat_n(padding as u8, padding));
    let cipher = Des::new(GenericArray::from_slice(key));
    for block in bytes.as_chunks_mut::<8>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    STANDARD.encode(bytes)
}
