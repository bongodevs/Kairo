use std::path::PathBuf;
use std::sync::Arc;

use abi_stable::std_types::{RResult, RString};
use futures_util::future::join_all;
use serde_json::{Value, json};

use super::*;
use crate::abi::{HostService, PluginFailure};
use crate::model::{HttpRequest, HttpResponse, Open, OpenRequest, Track, TrackInfo};

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
