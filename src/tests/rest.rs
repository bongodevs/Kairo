use std::collections::{BTreeMap, BTreeSet};

use super::test_support::{
    HttpResponse, app_state, app_state_with_sources, encoded_track, memory_sources, request,
    request_with_headers, spawn_server,
};
use crate::app::MAX_REQUEST_BODY;
use crate::players::FilterPolicy;
const AUTH: Option<&str> = Some("letmein");

fn json(response: &HttpResponse) -> serde_json::Value {
    serde_json::from_str(&response.body).expect("JSON response")
}

fn keys(value: &serde_json::Value) -> BTreeSet<&str> {
    value.as_object().expect("JSON object").keys().map(String::as_str).collect()
}

fn query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn assert_v4(response: &HttpResponse) {
    assert_eq!(response.header("lavalink-api-version"), Some("4"));
}

#[tokio::test]
async fn plugin_listing_and_reload_are_admin_routes() {
    let address = spawn_server(app_state(Some("letmein"))).await;

    let response = request(address, "GET", "/v4/plugins", None, None).await;
    assert_eq!(response.status, 401);

    let response = request(address, "GET", "/v4/plugins", AUTH, None).await;
    assert_eq!(response.status, 200);
    assert_eq!(json(&response), serde_json::json!([]));

    let response = request(address, "POST", "/v4/plugins/missing/reload", AUTH, None).await;
    assert_eq!(response.status, 404);
    let body = json(&response);
    assert!(body["message"].as_str().unwrap_or_default().contains("missing"));

    let response = request(address, "OPTIONS", "/v4/plugins", AUTH, None).await;
    assert_eq!(response.status, 200);
    assert!(response.header("allow").unwrap_or_default().contains("GET"));
}

#[tokio::test]
async fn auth_info_version_and_stats_match_the_v4_contract() {
    let address = spawn_server(app_state(Some("letmein"))).await;

    let response = request(address, "GET", "/v4/info", None, None).await;
    assert_eq!(response.status, 401);
    assert_v4(&response);
    let response = request(address, "GET", "/v4/info", Some("wrong"), None).await;
    assert_eq!(response.status, 403);
    assert_v4(&response);

    let version = request(address, "GET", "/version", AUTH, None).await;
    assert_eq!(version.status, 200);
    assert_eq!(version.body, env!("CARGO_PKG_VERSION"));
    assert_v4(&version);

    let info = request(address, "GET", "/v4/info", AUTH, None).await;
    assert_eq!(info.status, 200);
    assert_v4(&info);
    let info = json(&info);
    assert_eq!(
        keys(&info),
        [
            "buildTime",
            "filters",
            "git",
            "jvm",
            "lavaplayer",
            "plugins",
            "sourceManagers",
            "version",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(info["sourceManagers"], serde_json::json!([]));
    assert_eq!(
        info["filters"],
        serde_json::json!([
            "volume",
            "equalizer",
            "karaoke",
            "timescale",
            "tremolo",
            "vibrato",
            "distortion",
            "rotation",
            "channelMix",
            "lowPass",
            "high-pass",
            "low-pass",
            "normalization",
            "echo"
        ])
    );
    assert!(info["version"]["preRelease"].is_null());

    let stats = request(address, "GET", "/v4/stats", AUTH, None).await;
    assert_eq!(stats.status, 200);
    let stats = json(&stats);
    assert_eq!(stats["players"], 0);
    assert_eq!(stats["playingPlayers"], 0);
    assert!(stats.get("frameStats").is_none());
    assert_eq!(keys(&stats["memory"]), ["allocated", "free", "reservable", "used"].into());
    assert_eq!(keys(&stats["cpu"]), ["cores", "lavalinkLoad", "systemLoad"].into());
}

#[tokio::test]
async fn configured_filter_policy_controls_info_and_patch() {
    let mut state = app_state(Some("letmein"));
    state.filter_policy = FilterPolicy::configured(&BTreeMap::from([
        (String::from("volume"), false),
        (String::from("channelMix"), false),
        (String::from("high-pass"), false),
    ]));
    state.sessions.reserve_for("session", 1);
    let sessions = state.sessions.clone();
    let address = spawn_server(state).await;
    let info = request(address, "GET", "/v4/info", AUTH, None).await;
    assert_eq!(info.status, 200);
    let filters = json(&info)["filters"].as_array().cloned().expect("filters");

    assert!(!filters.contains(&serde_json::json!("volume")));
    assert!(!filters.contains(&serde_json::json!("channelMix")));
    assert!(!filters.contains(&serde_json::json!("high-pass")));
    assert!(filters.contains(&serde_json::json!("equalizer")));
    assert!(filters.contains(&serde_json::json!("echo")));

    let patch = request(
        address,
        "PATCH",
        "/v4/sessions/session/players/42",
        AUTH,
        Some(r#"{"filters":{"volume":1.0}}"#),
    )
    .await;
    assert_eq!(patch.status, 400);
    assert_eq!(json(&patch)["message"], "Following filters are disabled in the config: volume");
    assert_eq!(sessions.node_totals(), (0, 0));
}

#[tokio::test]
async fn a_forward_only_source_with_a_length_is_not_reported_as_a_stream() {
    let address = spawn_server(app_state_with_sources(Some("letmein"), memory_sources())).await;
    let load = request(address, "GET", "/v4/loadtracks?identifier=forward.wav", AUTH, None).await;
    assert_eq!(load.status, 200);
    let loaded = json(&load);
    assert_eq!(loaded["loadType"], "track");
    assert_eq!(loaded["data"]["info"]["isSeekable"], false);
    assert_eq!(loaded["data"]["info"]["isStream"], false);
    assert_eq!(loaded["data"]["info"]["length"], 50);
    assert!(loaded["data"]["encoded"].is_string());
}

#[tokio::test]
async fn load_and_track_codec_routes_keep_success_and_error_shapes() {
    let address = spawn_server(app_state(Some("letmein"))).await;
    let load = request(address, "GET", "/v4/loadtracks?identifier=missing", AUTH, None).await;
    assert_eq!(load.status, 200);
    assert_eq!(json(&load), serde_json::json!({"loadType": "empty", "data": null}));

    let encoded = encoded_track();
    let decoded = request(
        address,
        "GET",
        &format!("/v4/decodetrack?encodedTrack={}", query(&encoded)),
        AUTH,
        None,
    )
    .await;
    assert_eq!(decoded.status, 200);
    let decoded = json(&decoded);
    assert_eq!(decoded["encoded"], encoded);
    assert_eq!(decoded["info"]["sourceName"], "local");
    assert_eq!(decoded["pluginInfo"], serde_json::json!({}));
    assert_eq!(decoded["userData"], serde_json::json!({}));

    let body = serde_json::json!([encoded]).to_string();
    let many = request(address, "POST", "/v4/decodetracks", AUTH, Some(&body)).await;
    assert_eq!(many.status, 200);
    assert_eq!(json(&many).as_array().map(Vec::len), Some(1));

    let bad = request(address, "GET", "/v4/decodetrack?encodedTrack=garbage", AUTH, None).await;
    assert_eq!(bad.status, 500);
}

#[tokio::test]
async fn sessions_players_and_disabled_route_planner_match_the_contract() {
    let state = app_state(Some("letmein"));
    state.sessions.reserve_for("session", 123);
    let address = spawn_server(state).await;

    let session = request(
        address,
        "PATCH",
        "/v4/sessions/session",
        AUTH,
        Some(r#"{"resuming":true,"timeout":10}"#),
    )
    .await;
    assert_eq!(session.status, 200);
    assert_eq!(json(&session), serde_json::json!({"resuming": true, "timeout": 10}));

    let players = request(address, "GET", "/v4/sessions/session/players", AUTH, None).await;
    assert_eq!(players.status, 200);
    assert_eq!(json(&players), serde_json::json!([]));

    let player =
        request(address, "PATCH", "/v4/sessions/session/players/42", AUTH, Some("{}")).await;
    assert_eq!(player.status, 200);
    let player = json(&player);
    assert_eq!(player["guildId"], "42");
    assert!(player["track"].is_null());
    assert_eq!(player["state"]["connected"], false);
    assert_eq!(player["state"]["ping"], -1);

    for _ in 0..2 {
        let deleted =
            request(address, "DELETE", "/v4/sessions/session/players/42", AUTH, None).await;
        assert_eq!(deleted.status, 204);
    }

    let status = request(address, "GET", "/v4/routeplanner/status", AUTH, None).await;
    assert_eq!(status.status, 204);
    assert!(status.body.is_empty());
    let free = request(
        address,
        "POST",
        "/v4/routeplanner/free/address",
        AUTH,
        Some(r#"{"address":"127.0.0.1"}"#),
    )
    .await;
    assert_eq!(free.status, 500);
    assert_eq!(json(&free)["message"], "Can't access disabled route planner");
    let free = request(address, "POST", "/v4/routeplanner/free/all", AUTH, None).await;
    assert_eq!(free.status, 500);
}

#[tokio::test]
async fn methods_paths_content_types_and_cors_are_exact() {
    let address = spawn_server(app_state(Some("letmein"))).await;

    for (method, path) in [
        ("POST", "/version"),
        ("POST", "/v4/info"),
        ("POST", "/v4/stats"),
        ("POST", "/v4/loadtracks?identifier=missing"),
        ("POST", "/v4/decodetrack?encodedTrack=x"),
        ("GET", "/v4/decodetracks"),
        ("POST", "/v4/routeplanner/status"),
        ("GET", "/v4/routeplanner/free/address"),
        ("GET", "/v4/routeplanner/free/all"),
        ("GET", "/v4/sessions/session"),
        ("POST", "/v4/sessions/session/players"),
        ("POST", "/v4/sessions/session/players/42"),
    ] {
        let response = request(address, method, path, AUTH, None).await;
        assert_eq!(response.status, 405, "{method} {path}: {}", response.body);
        assert_eq!(response.header("content-type"), Some("application/json"));
    }

    let version = request_with_headers(
        address,
        "GET",
        "/version",
        AUTH,
        None,
        &[("Origin", "https://client.example")],
    )
    .await;
    assert_eq!(version.status, 200);
    assert_eq!(version.header("content-type"), Some("text/plain; charset=utf-8"));
    assert!(version.header("access-control-allow-origin").is_none());

    let head = request(address, "HEAD", "/version", AUTH, None).await;
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());

    for path in ["/v4/info/", "/V4/info", "/v4/Info"] {
        let response = request(address, "GET", path, AUTH, None).await;
        assert_eq!(response.status, 404, "GET {path}: {}", response.body);
    }
}

#[tokio::test]
async fn malformed_route_inputs_use_the_documented_statuses() {
    let address = spawn_server(app_state(Some("letmein"))).await;

    for path in ["/v4/loadtracks", "/v4/decodetrack"] {
        let response = request(address, "GET", path, AUTH, None).await;
        assert_eq!(response.status, 400, "GET {path}: {}", response.body);
    }

    let empty = request(address, "POST", "/v4/decodetracks", AUTH, Some("[]")).await;
    assert_eq!(empty.status, 400);
    assert_eq!(json(&empty)["message"], "No tracks to decode provided");

    let garbage = request(address, "POST", "/v4/decodetracks", AUTH, Some(r#"["garbage"]"#)).await;
    assert_eq!(garbage.status, 500, "{}", garbage.body);

    let wrong_type = request_with_headers(
        address,
        "POST",
        "/v4/decodetracks",
        AUTH,
        Some("[]"),
        &[("Content-Type", "text/plain")],
    )
    .await;
    assert_eq!(wrong_type.status, 415);
    assert_eq!(json(&wrong_type)["status"], 415);
}

#[tokio::test]
async fn a_body_over_the_documented_limit_is_rejected() {
    let address = spawn_server(app_state(Some("letmein"))).await;

    let at_limit = format!("[\"{}\"]", "a".repeat(MAX_REQUEST_BODY - 4));
    assert_eq!(at_limit.len(), MAX_REQUEST_BODY);
    let response = request(address, "POST", "/v4/decodetracks", AUTH, Some(&at_limit)).await;
    assert_ne!(response.status, 413, "the body limit must accept its own boundary");

    let oversized = format!("[\"{}\"]", "a".repeat(MAX_REQUEST_BODY));
    let response = request(address, "POST", "/v4/decodetracks", AUTH, Some(&oversized)).await;
    assert_eq!(response.status, 413, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let error = json(&response);
    assert_eq!(error["status"], 413);
    assert_eq!(error["error"], "Payload Too Large");
    assert_eq!(error["path"], "/v4/decodetracks");
}

#[tokio::test]
async fn root_and_the_old_websocket_endpoint_return_an_empty_200() {
    let address = spawn_server(app_state(Some("letmein"))).await;

    for path in ["/", "/v3/websocket"] {
        let response = request(address, "GET", path, AUTH, None).await;
        assert_eq!(response.status, 200, "{path}");
        assert!(response.body.is_empty(), "{path}: {}", response.body);
        assert_eq!(response.header("content-type"), None, "{path}");

        let unauthorized = request(address, "GET", path, None, None).await;
        assert_eq!(unauthorized.status, 401, "the old endpoint still requires auth");
    }
}

#[tokio::test]
async fn options_reports_allowed_methods_for_known_paths_only() {
    let state = app_state(Some("letmein"));
    state.sessions.reserve_for("session", 1);
    let address = spawn_server(state).await;

    let info = request(address, "OPTIONS", "/v4/info", AUTH, None).await;
    assert_eq!(info.status, 200);
    assert!(info.body.is_empty(), "{}", info.body);
    assert_eq!(info.header("allow"), Some("GET,HEAD,OPTIONS"));

    let session = request(address, "OPTIONS", "/v4/sessions/session", AUTH, None).await;
    assert_eq!(session.status, 200);
    assert_eq!(session.header("allow"), Some("PATCH,OPTIONS"));

    let player = request(address, "OPTIONS", "/v4/sessions/session/players/42", AUTH, None).await;
    assert_eq!(player.status, 200);
    let allow = player.header("allow").expect("allow header");
    for method in ["GET", "HEAD", "PATCH", "DELETE", "OPTIONS"] {
        assert!(allow.split(',').any(|value| value == method), "{allow} lacks {method}");
    }

    let unknown = request(address, "OPTIONS", "/v4/not-a-route", AUTH, None).await;
    assert_eq!(unknown.status, 404);
    assert_eq!(json(&unknown)["error"], "Not Found");

    let unauthorized = request(address, "OPTIONS", "/v4/info", None, None).await;
    assert_eq!(unauthorized.status, 401);
}

#[tokio::test]
async fn http2_is_only_served_when_enabled() {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, Version};
    use tower::ServiceExt;

    fn versioned(version: Version) -> Request<Body> {
        Request::builder()
            .version(version)
            .uri("/version")
            .header("authorization", "letmein")
            .body(Body::empty())
            .expect("request")
    }

    async fn body(response: axum::response::Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8 body")
    }

    let mut state = app_state(Some("letmein"));
    state.http2_enabled = false;
    let router = crate::app::router(state);

    let refused = router.clone().oneshot(versioned(Version::HTTP_2)).await.expect("response");
    assert_eq!(refused.status(), 505);
    assert_eq!(refused.headers()["lavalink-api-version"], "4");

    let normal = router.oneshot(versioned(Version::HTTP_11)).await.expect("response");
    assert_eq!(normal.status(), 200);
    assert_eq!(body(normal).await, env!("CARGO_PKG_VERSION"));

    let mut state = app_state(Some("letmein"));
    state.http2_enabled = true;
    let served =
        crate::app::router(state).oneshot(versioned(Version::HTTP_2)).await.expect("response");
    assert_eq!(served.status(), 200);
    assert_eq!(body(served).await, env!("CARGO_PKG_VERSION"));
}
