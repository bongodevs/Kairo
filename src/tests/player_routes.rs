use super::*;
use crate::test_support::{LogCapture, app_state, capture_subscriber};
fn state() -> AppState {
    let state = app_state(None);
    state.sessions.reserve("session");
    state
}

async fn json(response: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

#[tokio::test]
async fn get_list_and_idempotent_delete_use_the_session_registry() {
    let state = state();
    let guild_id: GuildId = "123".parse().expect("guild id");
    state
        .sessions
        .create_player(
            "session",
            guild_id,
            state.sources().clone(),
            state.codec_registry(),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    assert_eq!(state.sessions.node_totals(), (1, 0));

    let response = get_player(State(state.clone()), ApiPath((String::from("session"), 123))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["guildId"], "123");
    assert!(body["track"].is_null());
    assert_eq!(body["volume"], 100);
    assert_eq!(body["paused"], false);
    assert_eq!(body["state"]["connected"], false);
    assert_eq!(body["state"]["ping"], -1);
    assert_eq!(body["filters"]["pluginFilters"], serde_json::json!({}));

    let response = get_players(State(state.clone()), ApiPath(String::from("session"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await.as_array().map(Vec::len), Some(1));

    for _ in 0..2 {
        let response =
            delete_player(State(state.clone()), ApiPath((String::from("session"), 123))).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let response = get_player(State(state.clone()), ApiPath((String::from("session"), 123))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(state.sessions.node_totals(), (0, 0));
}

#[tokio::test]
async fn unknown_session_is_not_found() {
    let state = state();
    let response = get_players(State(state), ApiPath(String::from("missing"))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn player_create_and_destroy_are_logged_with_ids() {
    let capture = LogCapture::default();
    let _guard = tracing::subscriber::set_default(capture_subscriber(&capture));
    let state = state();
    let guild_id: GuildId = "123".parse().expect("guild id");

    state
        .sessions
        .create_player(
            "session",
            guild_id,
            state.sources().clone(),
            state.codec_registry(),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    let player =
        state.sessions.remove_player("session", guild_id).expect("session").expect("player");
    let _ = player.shutdown().await;

    let text = capture.text();
    assert!(text.contains("player created"), "{text}");
    assert!(text.contains("player destroyed"), "{text}");
    assert!(text.contains("session_id=\"session\""), "{text}");
    assert!(text.contains("guild_id=123"), "{text}");
}

#[tokio::test]
async fn a_parked_session_is_not_addressable_over_rest_until_it_resumes() {
    let state = state();
    let guild_id = GuildId::new(123);
    state
        .sessions
        .create_player(
            "session",
            guild_id,
            state.sources().clone(),
            state.codec_registry(),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    state.sessions.update("session", Some(true), Some(60));
    state.sessions.close("session");
    assert_eq!(state.sessions.paused(), 1);

    let response = get_players(State(state.clone()), ApiPath(String::from("session"))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = get_player(State(state.clone()), ApiPath((String::from("session"), 123))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response =
        delete_player(State(state.clone()), ApiPath((String::from("session"), 123))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    assert!(state.sessions.begin_resume("session"));
    let response = get_player(State(state.clone()), ApiPath((String::from("session"), 123))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let player =
        state.sessions.remove_player("session", guild_id).expect("session").expect("player");
    player.shutdown().await.expect("shutdown player");
}
