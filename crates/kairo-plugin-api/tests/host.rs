use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use abi_stable::std_types::{RResult, RString};
use futures_util::future::join_all;
use serde_json::{Value, json};

use super::*;
use crate::abi::{HostService, PluginFailure};
use crate::model::{
    HttpRequest, HttpResponse, LoadRequest, LoadResult, Open, OpenRequest, Track, TrackInfo,
};

struct Callbacks;

impl HostCallbacks for Callbacks {
    fn log(&self, _level: LogLevel, _message: &str) {}

    fn http<'a>(
        &'a self,
        _request: HttpRequest,
    ) -> HostFuture<'a, Result<HttpResponse, PluginError>> {
        Box::pin(async { Err(PluginError::fault("HTTP is not used by this fixture")) })
    }
}

fn callbacks() -> Arc<dyn HostCallbacks> {
    Arc::new(Callbacks)
}

struct HttpCallbacks;

impl HostCallbacks for HttpCallbacks {
    fn log(&self, _level: LogLevel, _message: &str) {}

    fn http<'a>(
        &'a self,
        request: HttpRequest,
    ) -> HostFuture<'a, Result<HttpResponse, PluginError>> {
        Box::pin(async move {
            Ok(HttpResponse {
                status: 200,
                headers: request.headers,
                body: request.body.unwrap_or_default(),
            })
        })
    }
}

/// Signals when a plugin call reaches the host, then never answers it.
struct HangingCallbacks(Arc<tokio::sync::Notify>);

impl HostCallbacks for HangingCallbacks {
    fn log(&self, _level: LogLevel, _message: &str) {}

    fn http<'a>(
        &'a self,
        _request: HttpRequest,
    ) -> HostFuture<'a, Result<HttpResponse, PluginError>> {
        Box::pin(async move {
            self.0.notify_one();
            std::future::pending().await
        })
    }
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug").join(format!(
        "{}sources{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

fn fixture_track(identifier: impl Into<String>) -> Track {
    Track::new(TrackInfo::new(identifier, "Song", "Artist", "jiosaavn", 180_000))
}

#[test]
fn json_decoding_keeps_the_response_cap() {
    let body = "x".repeat(MAX_RESPONSE_BYTES + 1);
    let error = decode_json::<Value>("call", &body).unwrap_err();

    assert!(matches!(error, HostError::ResponseTooLarge { .. }));
}

#[test]
fn json_decoding_accepts_bodies_within_the_cap() {
    let value: Value = decode_json("call", r#"{"ok":true}"#).unwrap();

    assert_eq!(value["ok"], true);
}

#[test]
fn plugin_error_body_keeps_severity_and_details() {
    let error = call_error("call", PluginFailure::unavailable("down"));

    let HostError::Call { operation, error } = error else {
        panic!("expected plugin call error");
    };
    assert_eq!(operation, "call");
    assert!(error.unavailable);
    assert_eq!(error.message, "down");
    assert_eq!(error.severity, crate::Severity::Suspicious);
}

#[test]
fn malformed_error_body_is_typed() {
    let error = decode_json::<Value>("call", "not-json").unwrap_err();

    assert!(matches!(error, HostError::ResponseJson { operation: "call", .. }));
}

#[test]
fn missing_library_is_a_typed_error() {
    let path = PathBuf::from("definitely-not-a-plugin.so");
    let error = match LoadedPlugin::load(&path, callbacks(), &json!({})) {
        Ok(_) => panic!("missing library loaded"),
        Err(error) => error,
    };

    assert!(matches!(error, HostError::Library { .. }));
}

#[test]
fn abi_mismatch_names_both_versions() {
    let error = check_abi(crate::ABI_VERSION + 1).unwrap_err();

    assert!(matches!(
        error,
        HostError::Abi { expected: crate::ABI_VERSION, actual } if actual == crate::ABI_VERSION + 1
    ));
}

#[test]
fn loaded_plugin_can_cross_worker_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LoadedPlugin>();
}

#[tokio::test]
async fn host_bridge_serializes_http_callbacks() {
    let bridge = HostBridge { callbacks: Arc::new(HttpCallbacks), runtime: None };
    let request = serde_json::to_string(&HttpRequest {
        method: crate::HttpMethod::Post,
        url: "https://example.test".into(),
        headers: [("x-demo".into(), "yes".into())].into(),
        body: Some("hello".into()),
    })
    .unwrap();

    let RResult::ROk(body) = bridge.http(RString::from(request)).await else {
        panic!("host bridge rejected the request");
    };
    let response: HttpResponse = serde_json::from_str(body.as_str()).unwrap();

    assert_eq!(response.headers["x-demo"], "yes");
    assert_eq!(response.body, "hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run after `cargo build -p sources`"]
async fn real_fixture_loads_calls_and_shuts_down() {
    let plugin = LoadedPlugin::load(
        fixture_path(),
        callbacks(),
        &json!({ "jiosaavn": { "secretKey": "12345678", "mirror": true } }),
    )
    .unwrap();
    assert_eq!(plugin.manifest().name, "sources");
    assert_eq!(plugin.manifest().sources[0].name, "jiosaavn");

    let calls = (0..4).map(|index| {
        let plugin = &plugin;
        async move {
            plugin
                .call::<_, Open>(
                    "track.open",
                    &OpenRequest {
                        source: "jiosaavn".into(),
                        track: fixture_track(format!("song-{index}")),
                        direct: false,
                    },
                )
                .await
        }
    });
    for call in join_all(calls).await {
        assert!(call.is_ok());
    }

    let opened: Open = plugin
        .call(
            "track.open",
            &OpenRequest {
                source: "jiosaavn".into(),
                track: fixture_track("song-final"),
                direct: false,
            },
        )
        .await
        .unwrap();
    let Open::Mirror { title, .. } = opened else {
        panic!("expected mirror playback");
    };
    assert_eq!(title, "Song");

    plugin.shutdown();
    plugin.shutdown();
    let error = plugin.call::<_, Value>("source.load", &json!({})).await.unwrap_err();
    assert!(matches!(error, HostError::Shutdown));
}

/// An idle instance has nothing to drain, so shutdown must not touch the drain window at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run after `cargo build -p sources`"]
async fn idle_shutdown_returns_before_the_drain_window() {
    let plugin = LoadedPlugin::load(fixture_path(), callbacks(), &json!({})).unwrap();

    let started = Instant::now();
    let clean = plugin.shutdown();

    assert!(clean);
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "idle shutdown waited {:?}",
        started.elapsed()
    );
}

/// A call that never returns must not hold the process open for the full HTTP timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run after `cargo build -p sources`"]
async fn shutdown_is_bounded_while_a_call_hangs() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let plugin = Arc::new(
        LoadedPlugin::load(
            fixture_path(),
            Arc::new(HangingCallbacks(Arc::clone(&entered))),
            &json!({ "jiosaavn": { "secretKey": "12345678" } }),
        )
        .unwrap(),
    );
    let call = {
        let plugin = Arc::clone(&plugin);
        tokio::spawn(async move {
            plugin
                .call::<_, LoadResult>(
                    "source.load",
                    &LoadRequest { source: "jiosaavn".into(), identifier: "jssearch:never".into() },
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the hung call never reached the host");

    let started = Instant::now();
    let clean = tokio::task::spawn_blocking({
        let plugin = Arc::clone(&plugin);
        move || plugin.shutdown()
    })
    .await
    .unwrap();
    let waited = started.elapsed();

    assert!(!clean, "a hung call must be abandoned, not reported as clean");
    assert!(waited < Duration::from_secs(5), "shutdown waited {waited:?}");
    call.abort();
}

/// Current resident set size in KiB, or 0 where `/proc/self/status` is unavailable.
fn resident_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?.split_whitespace().next()?.parse().ok()
            })
        })
        .unwrap_or(0)
}

/// abi_stable never unloads a library, so a load/call/retire cycle may retain the library path and
/// the root module, but it must not grow with each cycle. 200 cycles is enough for a per-cycle
/// allocation to be obvious against allocator noise.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run after `cargo build -p sources`"]
async fn repeated_load_call_retire_retains_only_the_library() {
    let path = fixture_path();
    let config = json!({ "jiosaavn": { "secretKey": "12345678", "mirror": true } });
    let open = |identifier: &str| OpenRequest {
        source: "jiosaavn".into(),
        track: fixture_track(identifier),
        direct: false,
    };

    // Warm the one-time library load and root module so they are not counted as per-cycle growth.
    let plugin = LoadedPlugin::load(&path, callbacks(), &config).unwrap();
    plugin.call::<_, Open>("track.open", &open("warm-up")).await.unwrap();
    plugin.shutdown();
    drop(plugin);

    const CYCLES: u64 = 200;
    let before = resident_kib();
    for index in 0..CYCLES {
        let plugin = LoadedPlugin::load(&path, callbacks(), &config).unwrap();
        plugin.call::<_, Open>("track.open", &open(&format!("song-{index}"))).await.unwrap();
        plugin.shutdown();
        drop(plugin);
    }
    let after = resident_kib();
    let growth = after.saturating_sub(before);

    eprintln!(
        "VmRSS {before} KiB -> {after} KiB over {CYCLES} load/call/retire cycles: \
         {growth} KiB total, {} KiB per cycle",
        growth / CYCLES
    );
    // A per-cycle leak of the 97-byte path and 32-byte root would stay under this, so the bound
    // catches allocator-scale growth rather than the documented retention. The printed number is
    // the measurement that matters.
    assert!(growth < 32 * 1024, "unexpected {growth} KiB growth over {CYCLES} cycles");
}

#[test]
fn mutation_test_plugin_boundary_parse_points_never_panic_or_hang() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0
        }
        fn next_usize(&mut self, max: usize) -> usize {
            if max == 0 { 0 } else { (self.next() as usize) % max }
        }
    }

    let seed: u64 = 0xFEED_FACE_9876_5432;
    let mut rng = Rng(seed);

    let manifest_seed = br#"{"name":"test","version":"1.0.0","sources":["test"]}"#.to_vec();
    let load_result_seed = br#"{"type":"empty"}"#.to_vec();
    let open_seed = br#"{"type":"url","url":"https://example.com/audio.mp3"}"#.to_vec();
    let seeds = [manifest_seed, load_result_seed, open_seed];

    for iteration in 0..100_000 {
        let base = &seeds[rng.next_usize(seeds.len())];
        let mut mutated = base.clone();

        match rng.next() % 4 {
            0 => {
                let count = 1 + rng.next_usize(4);
                for _ in 0..count {
                    if !mutated.is_empty() {
                        let idx = rng.next_usize(mutated.len());
                        mutated[idx] ^= (rng.next() & 0xFF) as u8;
                    }
                }
            }
            1 => {
                let new_len = rng.next_usize(mutated.len() + 1);
                mutated.truncate(new_len);
            }
            2 => {
                let extra = 1 + rng.next_usize(64);
                for _ in 0..extra {
                    mutated.push((rng.next() & 0xFF) as u8);
                }
            }
            3 => {
                if !mutated.is_empty() {
                    let start = rng.next_usize(mutated.len());
                    let count = rng.next_usize(mutated.len() - start);
                    for byte in mutated.iter_mut().skip(start).take(count) {
                        *byte = (rng.next() & 0xFF) as u8;
                    }
                }
            }
            _ => unreachable!(),
        }

        let input_str = String::from_utf8_lossy(&mutated);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match iteration % 3 {
                0 => {
                    let _ = decode_json::<Manifest>("manifest", &input_str);
                }
                1 => {
                    let _ = decode_json::<LoadResult>("load", &input_str);
                }
                _ => {
                    let _ = decode_json::<Open>("open", &input_str);
                }
            }));
        if result.is_err() {
            panic!(
                "decode_json panicked at iteration {iteration} with seed {seed:#x}, input: {input_str}"
            );
        }
    }
}
