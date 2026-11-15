use super::*;
use crate::test_support::{
    app_state_with_sources, encoded_track, memory_sources, request as http_request, spawn_server,
};
async fn patch_player(address: SocketAddr, session_id: &str, body: &str) {
    let response = http_request(
        address,
        "PATCH",
        &format!("/v4/sessions/{session_id}/players/42"),
        Some("letmein"),
        Some(body),
    )
    .await;
    assert_eq!(response.status, 200, "PATCH response: {}", response.body);
}

#[tokio::test]
async fn player_patch_produces_the_v4_lifecycle_sequence() {
    let state = app_state_with_sources(Some("letmein"), memory_sources());
    let address = spawn_server(state).await;
    let mut stream = connect(address).await;
    let ready = next_json(&mut stream).await;
    let session_id = ready["sessionId"].as_str().expect("session id");
    assert_eq!(ready["op"], "ready");
    assert_eq!(next_json(&mut stream).await["op"], "stats");

    let encoded = encoded_track();
    let play = serde_json::json!({
        "track": {"encoded": encoded},
        "paused": true,
    })
    .to_string();
    patch_player(address, session_id, &play).await;

    let mut starts = 0;
    let mut updates = 0;
    let mut event_track = None;
    for _ in 0..3 {
        let message = next_json(&mut stream).await;
        match (message["op"].as_str(), message["type"].as_str()) {
            (Some("event"), Some("TrackStartEvent")) => {
                starts += 1;
                assert_eq!(message["guildId"], "42");
                event_track = message["track"]["encoded"].as_str().map(str::to_owned);
            }
            (Some("playerUpdate"), _) => {
                updates += 1;
                assert_eq!(message["guildId"], "42");
                assert_eq!(message["state"]["position"], 0);
                assert_eq!(message["state"]["connected"], false);
                assert_eq!(message["state"]["ping"], -1);
            }
            _ => panic!("unexpected player message: {message}"),
        }
    }
    assert_eq!((starts, updates), (1, 2));
    let event_track = event_track.expect("canonical event track");

    patch_player(address, session_id, r#"{"position":20}"#).await;
    let seek = next_json(&mut stream).await;
    assert_eq!(seek["op"], "playerUpdate");
    assert!(seek["state"]["position"].is_u64());

    patch_player(address, session_id, r#"{"filters":{"volume":0.5}}"#).await;
    let filter = next_json(&mut stream).await;
    assert_eq!(filter["op"], "playerUpdate");
    assert_eq!(filter["guildId"], "42");

    patch_player(address, session_id, r#"{"track":{"encoded":null}}"#).await;
    let end = next_json(&mut stream).await;
    assert_eq!(end["op"], "event");
    assert_eq!(end["type"], "TrackEndEvent");
    assert_eq!(end["guildId"], "42");
    assert_eq!(end["reason"], "stopped");
    assert_eq!(end["track"]["encoded"], event_track);
}
