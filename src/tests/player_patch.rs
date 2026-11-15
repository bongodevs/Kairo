use axum::body::to_bytes;

use player::source::SourceReference;
use player::track::codec::{self, TrackCodecs};
use player::track::{SourceData, Track, TrackInfo};

use super::*;
use crate::rest::routes::players::{delete_player, get_player};
use crate::session::{SessionSink, UpdateQueue};
use crate::test_support::{app_state_with_sources, memory_sources};
fn encoded_track() -> String {
    let track = Track::from_parts(
        SourceReference::new("memory.wav"),
        TrackInfo {
            identifier: String::from("memory.wav"),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(50)),
            is_stream: false,
            title: String::from("Track"),
            uri: Some(String::from("memory.wav")),
            source_name: String::from("local"),
            artwork_url: None,
            isrc: None,
        },
        SourceData::probe("wav", None),
        Millis::default(),
    );
    codec::encode(&track.snapshot(Millis::default()), &TrackCodecs::new()).expect("encode")
}

fn outbound(capacity: usize) -> (SessionSink, tokio::sync::mpsc::Receiver<String>) {
    let (messages, receiver) = tokio::sync::mpsc::channel(capacity);
    let (cancel, _) = tokio::sync::watch::channel(None);
    (
        SessionSink { messages, updates: std::sync::Arc::new(UpdateQueue::default()), cancel },
        receiver,
    )
}

fn state() -> AppState {
    let mut state = app_state_with_sources(None, memory_sources());
    state.player_config.frame_buffer = player::buffer::FrameBufferConfig::new(200).expect("buffer");
    state.sessions.reserve_for("session", 789);
    state
}

async fn patch(state: AppState, body: serde_json::Value, no_replace: bool) -> Response {
    patch_player(
        State(state),
        ApiPath((String::from("session"), 123)),
        ApiQuery(PatchQuery { no_replace }),
        ApiJson(serde_json::from_value(body).expect("update body")),
    )
    .await
}

async fn json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 128 * 1024).await.expect("body");
    serde_json::from_slice(&bytes).expect("JSON")
}

async fn next_message(events: &mut tokio::sync::mpsc::Receiver<String>) -> serde_json::Value {
    let message = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .expect("player message timeout")
        .expect("player message");
    serde_json::from_str(&message).expect("player message JSON")
}

async fn next_event(
    events: &mut tokio::sync::mpsc::Receiver<String>,
    event_type: &str,
) -> serde_json::Value {
    loop {
        let message = next_message(events).await;
        if message["type"] == event_type {
            return message;
        }
    }
}

async fn next_player_update(events: &mut tokio::sync::mpsc::Receiver<String>) -> serde_json::Value {
    loop {
        let message = next_message(events).await;
        if message["op"] == "playerUpdate" {
            return message;
        }
    }
}

#[test]
fn explicit_null_track_is_distinct_from_an_omitted_track() {
    let update: PlayerUpdate = serde_json::from_value(serde_json::json!({
        "track": {"encoded": null}
    }))
    .expect("update");
    let Omissible::Present(track) = update.track else {
        panic!("track should be present");
    };
    assert!(matches!(track.encoded, Omissible::Present(None)));
}

#[tokio::test]
async fn patch_creates_updates_and_honours_no_replace() {
    let mut state = state();
    state.metrics_endpoint = Some(String::from("/metrics"));
    let (outbound, mut events) = outbound(8);
    assert!(state.sessions.set_outbound("session", outbound));
    let encoded = encoded_track();
    let response = patch(
        state.clone(),
        serde_json::json!({
            "track": {
                "encoded": encoded,
                "userData": {"requester": "42"}
            },
            "paused": true,
            "volume": 250,
            "filters": {
                "volume": 0.5,
                "pluginFilters": {
                    "high-pass": {"cutoffFrequency": 120}
                }
            }
        }),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["guildId"], "123");
    assert_eq!(body["volume"], 250);
    assert_eq!(body["paused"], true);
    assert_eq!(body["track"]["userData"]["requester"], "42");
    assert!(body["voice"]["channelId"].is_null());
    assert!(body["filters"]["pluginFilters"].get("high-pass").is_some());
    let mut starts = 0;
    let mut updates = 0;
    for _ in 0..4 {
        let message = next_message(&mut events).await;
        match (message["op"].as_str(), message["type"].as_str()) {
            (Some("event"), Some("TrackStartEvent")) => {
                starts += 1;
                assert_eq!(message["guildId"], "123");
            }
            (Some("playerUpdate"), _) => {
                updates += 1;
                assert_eq!(message["guildId"], "123");
                assert_eq!(message["state"]["connected"], false);
                assert_eq!(message["state"]["ping"], -1);
            }
            _ => panic!("unexpected player message: {message}"),
        }
    }
    assert_eq!(starts, 1);
    assert_eq!(updates, 3, "filter, play and track start each push state");
    assert!(
        state.search_metrics.play_samples().contains(&String::from(
            r#"lavalink_tracks_played_total{source="local",guild_id="123"} 1"#
        )),
        "the started track must feed the played counter"
    );
    assert_eq!(state.sessions.node_totals(), (1, 0), "a paused track is not a playing player");

    let players = state.sessions.players("session").expect("session players");
    let periodic = crate::players::active_player_update_messages(&players).await;
    assert_eq!(periodic.len(), 1, "a paused track remains periodically active");

    let response = patch(
        state.clone(),
        serde_json::json!({
            "track": {"encoded": null},
            "volume": 300
        }),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["volume"], 300);
    assert!(body["track"].is_object(), "noReplace must retain the track");
    assert!(events.try_recv().is_err(), "volume and noReplace do not add a state push");

    let response = patch(
        state.clone(),
        serde_json::json!({
            "track": {"userData": {"requester": "updated"}},
            "paused": false,
            "position": 20,
            "endTime": 40
        }),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["paused"], false);
    assert_eq!(body["track"]["userData"]["requester"], "updated");
    let update = next_player_update(&mut events).await;
    assert_eq!(update["guildId"], "123", "a successful seek pushes state");
    assert_eq!(state.sessions.node_totals(), (1, 1));

    let response =
        delete_player(State(state.clone()), ApiPath((String::from("session"), 123))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(state.sessions.node_totals(), (0, 0));
    let event = next_event(&mut events, "TrackEndEvent").await;
    assert_eq!(event["type"], "TrackEndEvent");
    assert_eq!(event["reason"], "stopped");
}

#[tokio::test]
async fn next_track_and_crossfade_round_trip_through_the_response() {
    let state = state();
    let encoded = encoded_track();
    let response = patch(
        state.clone(),
        serde_json::json!({"track": {"encoded": encoded.clone()}, "paused": true}),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert!(body["nextTrack"].is_null());
    assert_eq!(body["crossfade"]["enable"], false);
    assert_eq!(body["crossfade"]["gapless"], true);

    let response = patch(
        state.clone(),
        serde_json::json!({
            "nextTrack": {"encoded": encoded},
            "crossfade": {"enable": true, "durationMs": 400, "gapless": true}
        }),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert!(
        body["nextTrack"]["encoded"].is_string(),
        "nextTrack must be listed like the current track"
    );
    assert_eq!(body["crossfade"]["enable"], true);
    assert_eq!(body["crossfade"]["durationMs"], 400);
    assert_eq!(body["crossfade"]["curve"], "sCurve");

    let response =
        patch(state.clone(), serde_json::json!({"nextTrack": null, "crossfade": null}), false)
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert!(body["nextTrack"].is_null());
    assert_eq!(body["crossfade"]["enable"], false);
    assert_eq!(body["crossfade"]["gapless"], true);
}

#[tokio::test]
async fn transition_needs_a_prepared_successor() {
    let state = state();
    let response = patch(
        state.clone(),
        serde_json::json!({"track": {"encoded": encoded_track()}, "paused": true}),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = patch(state.clone(), serde_json::json!({"transition": true}), false).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = patch(
        state.clone(),
        serde_json::json!({
            "track": {"encoded": encoded_track()},
            "transition": true
        }),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn validation_happens_before_player_creation() {
    let state = state();
    for body in [
        serde_json::json!({
            "track": {"encoded": null},
            "encodedTrack": null
        }),
        serde_json::json!({
            "track": {"encoded": null, "identifier": "memory.wav"}
        }),
        serde_json::json!({"endTime": 0}),
        serde_json::json!({
            "voice": {"token": "", "endpoint": "x", "sessionId": "y", "channelId": "z"}
        }),
    ] {
        let response = patch(state.clone(), body, false).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(state.sessions.players("session").expect("session").list().is_empty());
}

#[tokio::test]
async fn identifier_and_deprecated_encoded_track_forms_both_resolve() {
    let state = state();
    let response = patch(
        state.clone(),
        serde_json::json!({
            "track": {"identifier": "memory.wav", "userData": {"form": "identifier"}},
            "paused": true
        }),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["track"]["info"]["identifier"], "memory.wav");
    assert_eq!(body["track"]["userData"]["form"], "identifier");

    let response =
        patch(state, serde_json::json!({"encodedTrack": encoded_track(), "paused": true}), false)
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["track"]["info"]["identifier"], "memory.wav");
}

#[tokio::test]
async fn identifier_rejects_empty_playlist_and_search_results() {
    let state = state();
    for (identifier, message) in [
        ("missing", "No matches found for identifier"),
        ("playlist", "Cannot play a playlist or search result"),
        ("search", "Cannot play a playlist or search result"),
    ] {
        let response =
            patch(state.clone(), serde_json::json!({"track": {"identifier": identifier}}), false)
                .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(String::from_utf8_lossy(&body), message);
    }
}

#[tokio::test]
async fn voice_connection_failure_is_a_server_error_and_keeps_the_player_disconnected() {
    let state = state();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        patch(
            state.clone(),
            serde_json::json!({
                "voice": {
                    "token": "token",
                    "endpoint": "127.0.0.1:9",
                    "sessionId": "voice-session",
                    "channelId": "456"
                }
            }),
            false,
        ),
    )
    .await
    .expect("voice connect timeout");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let response = get_player(State(state), ApiPath((String::from("session"), 123))).await;
    let body = json(response).await;
    assert_eq!(body["state"]["connected"], false);
    assert_eq!(body["state"]["ping"], -1);
}

#[tokio::test]
async fn a_skip_transition_ends_the_current_track_and_starts_the_successor() {
    let state = state();
    let (outbound, mut events) = outbound(64);
    assert!(state.sessions.set_outbound("session", outbound));
    let current = crate::test_support::long_encoded_track("Long Track");
    let successor = crate::test_support::long_encoded_track("Next Track");

    // gurulink's Play: the track and the crossfade settings in one update.
    let response = patch(
        state.clone(),
        serde_json::json!({
            "track": {"encoded": current.clone()},
            "paused": false,
            "crossfade": {
                "enable": true,
                "durationMs": 400,
                "manualDurationMs": 100,
                "gapless": true
            }
        }),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // No voice connection in this test, so drain the frames the worker would otherwise block on.
    let entry = state
        .sessions
        .players("session")
        .expect("session players")
        .get(crate::players::GuildId::new(123))
        .expect("player");
    let mut frames = entry.take_frame_consumer().expect("frame consumer");
    let drain = tokio::spawn(async move {
        let mut output = [0u8; player::audio::MAX_PACKET_BYTES];
        while let Ok(read) = frames.read(&mut output) {
            if matches!(read, player::buffer::BufferRead::End | player::buffer::BufferRead::Closed)
            {
                break;
            }
            // Roughly realtime: the worker produces ahead of the speaker, so an unpaced drain
            // would run the whole track dry before the next command lands.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    // gurulink's debounced pre-buffer names the successor alone.
    let response = patch(
        state.clone(),
        serde_json::json!({"nextTrack": {"encoded": successor.clone()}}),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // gurulink's Skip: the successor plus transition, without the crossfade settings.
    let response = patch(
        state.clone(),
        serde_json::json!({
            "nextTrack": {"encoded": successor.clone()},
            "transition": true
        }),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let end = next_event(&mut events, "TrackEndEvent").await;
    assert!(
        matches!(end["reason"].as_str(), Some("crossfade" | "gapless")),
        "the skip must end the outgoing track with a transition reason: {end}"
    );
    // gurulink guards its queue on the encoded string, so an event must carry the exact bytes the
    // client queued rather than a re-encoding at the live playback position.
    assert_eq!(end["track"]["encoded"], current);

    let start = next_event(&mut events, "TrackStartEvent").await;
    assert_eq!(start["type"], "TrackStartEvent");
    assert_eq!(start["track"]["encoded"], successor);

    let player = state
        .sessions
        .players("session")
        .expect("session players")
        .get(crate::players::GuildId::new(123))
        .expect("player");
    assert!(player.next_track().is_none(), "the promoted successor must be retired from nextTrack");
    drain.abort();
}
