use std::collections::BTreeMap;

use axum::body::to_bytes;

use super::*;
use crate::players::FilterPolicy;
use crate::test_support::app_state;
fn state() -> AppState {
    let state = app_state(None);
    state.sessions.reserve_for("session", 789);
    state
}

async fn patch(state: AppState, body: serde_json::Value) -> Response {
    patch_player(
        State(state),
        ApiPath((String::from("session"), 123)),
        ApiQuery(PatchQuery { no_replace: false }),
        ApiJson(serde_json::from_value(body).expect("update body")),
    )
    .await
}

async fn text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.expect("body");
    String::from_utf8(bytes.to_vec()).expect("UTF-8 response")
}

#[tokio::test]
async fn disabled_and_invalid_filters_are_rejected_before_player_creation() {
    let mut state = state();
    state.filter_policy = FilterPolicy::configured(&BTreeMap::from([
        (String::from("volume"), false),
        (String::from("karaoke"), false),
        (String::from("high-pass"), false),
    ]));
    let response = patch(
        state.clone(),
        serde_json::json!({
            "filters": {
                "volume": 1.0,
                "karaoke": null,
                "pluginFilters": {"high-pass": {"cutoffFrequency": 120}}
            },
            "voice": {"token": "", "endpoint": "", "sessionId": "", "channelId": null}
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        text(response).await,
        "Following filters are disabled in the config: volume, karaoke, high-pass"
    );

    state.filter_policy = FilterPolicy::default();
    let response = patch(state.clone(), serde_json::json!({"filters": {"volume": 6.0}})).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text(response).await, "Invalid filter configuration");
    assert!(state.sessions.players("session").expect("session").list().is_empty());
}

#[tokio::test]
async fn invalid_filter_values_are_rejected_before_player_creation() {
    let state = state();
    for filters in [
        serde_json::json!({"volume": -1.0}),
        serde_json::json!({"volume": 5.5}),
        serde_json::json!({"tremolo": {"frequency": 0.0, "depth": 0.5}}),
        serde_json::json!({"tremolo": {"frequency": 2.0, "depth": 2.0}}),
        serde_json::json!({"vibrato": {"frequency": 0.0, "depth": 0.5}}),
        serde_json::json!({"vibrato": {"frequency": 20.0, "depth": 0.5}}),
        serde_json::json!({"vibrato": {"frequency": 2.0, "depth": -1.0}}),
    ] {
        let response = patch(state.clone(), serde_json::json!({ "filters": filters })).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{filters}");
        assert_eq!(text(response).await, "Invalid filter configuration");
    }
    assert!(state.sessions.players("session").expect("session").list().is_empty());
}

#[tokio::test]
async fn out_of_range_equalizer_bands_are_safely_ignored() {
    let state = state();
    let response = patch(
        state,
        serde_json::json!({
            "filters": {
                "equalizer": [
                    {"band": 99, "gain": 9.0},
                    {"band": 3, "gain": 9.0}
                ]
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}
