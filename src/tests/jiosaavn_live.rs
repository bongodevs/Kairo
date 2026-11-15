//! Opt-in live check for the real JioSaavn service. It reads the operator's local config, skips when
//! JioSaavn is absent, and never prints the key. Run with:
//! `cargo test -p kairo live_jiosaavn -- --ignored --nocapture`

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use player::audio::MAX_PACKET_BYTES;
use player::buffer::{BufferRead, FrameBufferConfig};
use player::playback::{PlayerConfig, spawn_player};
use player::source::{HTTP_SOURCE, LOCAL_SOURCE, ProviderManager};
use player::track::codec::DecodeTrack;

use crate::config::{Config, resolve_providers};
use crate::plugins::PluginManager;
use crate::test_support::{app_state_with_shared_sources, request, spawn_server};

#[tokio::test]
#[ignore = "calls the live JioSaavn service with the operator's local config"]
async fn live_jiosaavn_load_decode_and_frames() {
    let path = std::env::var("KAIRO_CONFIG").unwrap_or_else(|_| String::from("config.local.toml"));
    let Ok(config) = Config::load(std::path::Path::new(&path)) else {
        eprintln!("no {path}, skipping live check");
        return;
    };
    let Some(plugin) = config.plugins.iter().find(|plugin| plugin.name == "sources").cloned()
    else {
        eprintln!("sources plugin is not configured, skipping live check");
        return;
    };
    let manager =
        PluginManager::load(&[plugin], Path::new("plugins")).await.expect("plugin manager");
    let source_config = config.lavalink.server.sources.source_config(&config.sources);
    let fallbacks = resolve_providers(
        &config.sources.providers,
        manager.source_names().chain(
            [(source_config.local, LOCAL_SOURCE), (source_config.http, HTTP_SOURCE)]
                .into_iter()
                .filter_map(|(enabled, name)| enabled.then_some(name)),
        ),
    )
    .expect("fallback providers");
    let mut providers = ProviderManager::new().with_probe(player::source_probe::audio_probe());
    let mirror = manager
        .register_sources(&mut providers, [HTTP_SOURCE, LOCAL_SOURCE], fallbacks)
        .expect("register sources plugin");
    let sources = Arc::new(providers.build(source_config).expect("sources"));
    mirror.install(&sources);
    let state = app_state_with_shared_sources(Some("live"), Arc::clone(&sources));
    let playback_sources = state.sources().clone();
    let address = spawn_server(state).await;

    let loaded = request(
        address,
        "GET",
        "/v4/loadtracks?identifier=jssearch%3ATum%20Hi%20Ho",
        Some("live"),
        None,
    )
    .await;
    assert_eq!(loaded.status, 200, "load failed: {}", loaded.body);
    let loaded: serde_json::Value = serde_json::from_str(&loaded.body).expect("load JSON");
    assert_eq!(loaded["loadType"], "search", "load shape: {loaded}");
    let first = &loaded["data"][0];
    let encoded = first["encoded"].as_str().expect("encoded").to_owned();
    eprintln!(
        "loaded {} | {} | {} ms",
        first["info"]["title"], first["info"]["author"], first["info"]["length"]
    );

    let request_body = serde_json::to_string(&vec![encoded.clone()]).expect("request JSON");
    let decoded =
        request(address, "POST", "/v4/decodetracks", Some("live"), Some(&request_body)).await;
    assert_eq!(decoded.status, 200, "decode failed: {}", decoded.body);
    let decoded: serde_json::Value = serde_json::from_str(&decoded.body).expect("decode JSON");
    assert_eq!(decoded[0]["info"]["sourceName"], "jiosaavn");
    eprintln!("decoded {} | {}", decoded[0]["info"]["title"], decoded[0]["info"]["identifier"]);

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
    let mut packets = 0_u32;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match frames.read(&mut packet).expect("frame read") {
                BufferRead::Frame(_) => {
                    packets += 1;
                    if packets >= 50 {
                        break;
                    }
                }
                BufferRead::Empty => tokio::task::yield_now().await,
                BufferRead::End | BufferRead::Closed => panic!("playback ended before audio"),
            }
        }
    })
    .await
    .expect("audio timeout");
    eprintln!("player produced {packets} opus packets");
    guard.shutdown().await.expect("shutdown");
    manager.shutdown().await;
}
