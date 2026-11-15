use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tokio::sync::mpsc;

use super::owner::PlayerOwner;
use super::test_support::{OpusPlayer, next_frame};
use super::*;
use crate::audio::MAX_PACKET_BYTES;
use crate::buffer::{FrameBufferConfig, frame_buffer};
use crate::decode::Millis;
use crate::filters::Filters;
use crate::source::{SourceReference, Sources};
use crate::track::{SourceData, Track, TrackInfo, UNKNOWN_AUTHOR, UNKNOWN_TITLE};
#[tokio::test]
async fn player_state_reports_the_last_consumed_frame_position() {
    let mut fixture = OpusPlayer::new("position", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let _ = next_frame(&mut fixture.frames).await;
    let (second, _) = next_frame(&mut fixture.frames).await;
    assert_eq!(second.position, Millis::new(20));
    assert_eq!(fixture.player.state().await.expect("state").position, Millis::new(20));
    fixture
        .player
        .set_filters(Filters { volume: 0.5, ..Filters::default() })
        .await
        .expect("reconfigure");
    assert_eq!(fixture.player.state().await.expect("reconfigured state").position, Millis::new(20));

    fixture.player.stop().await.expect("stop");
    assert_eq!(fixture.player.state().await.expect("stopped state").position, Millis::default());
    fixture.shutdown().await;
}

#[tokio::test]
async fn track_start_and_state_carry_resolved_track_information() {
    let mut fixture = OpusPlayer::new("track-info", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let Some(PlayerEvent::TrackStart { track }) = fixture.events.recv().await else {
        panic!("expected track start");
    };
    let info = track.info().expect("resolved track info");
    assert_eq!(info.identifier, fixture.track.identifier());
    assert_eq!(info.source_name, "local");
    assert_eq!(info.title, UNKNOWN_TITLE);
    assert_eq!(info.author, UNKNOWN_AUTHOR);
    assert_eq!(info.length, Some(Millis::new(800)));
    assert!(info.is_seekable);
    assert!(!info.is_stream);
    assert_eq!(info.uri.as_deref(), Some(fixture.track.identifier()));
    assert_eq!(
        fixture.player.state().await.expect("state").track.expect("active track").info(),
        Some(info)
    );
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn decoded_track_initial_position_is_applied_before_track_start() {
    let mut fixture = OpusPlayer::new("initial-position", 40).await;
    let track = Track::from_parts(
        fixture.track.clone(),
        TrackInfo {
            identifier: fixture.track.identifier().to_owned(),
            is_seekable: true,
            author: String::from("Encoded Artist"),
            length: Some(Millis::new(777)),
            is_stream: false,
            title: String::from("Encoded Title"),
            uri: Some(fixture.track.identifier().to_owned()),
            source_name: String::from("local"),
            artwork_url: None,
            isrc: None,
        },
        SourceData::probe("ogg", None),
        Millis::new(200),
    );
    fixture.player.play(track, false).await.expect("play");
    let Some(PlayerEvent::TrackStart { track }) = fixture.events.recv().await else {
        panic!("expected track start");
    };
    assert_eq!(track.position(), Millis::new(200));
    assert_eq!(track.info().map(|info| info.title.as_str()), Some("Encoded Title"));
    assert_eq!(track.info().map(|info| info.author.as_str()), Some("Encoded Artist"));
    assert_eq!(track.info().and_then(|info| info.length), Some(Millis::new(777)));
    let (first, _) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position, Millis::new(200));
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn user_data_updates_only_the_active_track() {
    let fixture = OpusPlayer::new("user-data", 40).await;
    assert!(!fixture.player.set_user_data(Map::new()).await.expect("idle update"));
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let mut user_data = Map::new();
    user_data.insert(String::from("requester"), Value::String(String::from("42")));
    assert!(fixture.player.set_user_data(user_data.clone()).await.expect("active update"));
    let state = fixture.player.state().await.expect("state");
    assert_eq!(state.track.expect("track").user_data(), &user_data);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn an_unread_event_queue_never_blocks_player_commands() {
    let mut fixture = OpusPlayer::new("event-overflow", 40).await;
    for _ in 0..10 {
        tokio::time::timeout(
            Duration::from_secs(2),
            fixture.player.play(fixture.track.clone(), false),
        )
        .await
        .expect("play command timed out")
        .expect("play");
        tokio::time::timeout(Duration::from_secs(2), fixture.player.stop())
            .await
            .expect("stop command timed out")
            .expect("stop");
    }
    tokio::time::timeout(Duration::from_secs(2), fixture.player.state())
        .await
        .expect("state command timed out")
        .expect("state");

    let mut retained = 0;
    while fixture.events.try_recv().is_ok() {
        retained += 1;
    }
    assert_eq!(retained, 16, "the bounded queue keeps its oldest events");
    fixture.shutdown().await;
}

#[tokio::test]
async fn an_abandoned_track_ends_with_cleanup() {
    let config = PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
        cleanup_threshold_ms: 50,
        ..PlayerConfig::default()
    };
    let mut fixture = OpusPlayer::with_config("cleanup", 100, config).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(matches!(fixture.events.recv().await, Some(PlayerEvent::TrackStart { .. })));

    let event = tokio::time::timeout(Duration::from_secs(2), fixture.events.recv())
        .await
        .expect("cleanup timed out");
    assert!(matches!(
        event,
        Some(PlayerEvent::TrackEnd { track, reason: TrackEndReason::Cleanup })
            if track == fixture.track
    ));
    assert_eq!(fixture.player.state().await.expect("state").track, None);
    assert!(!TrackEndReason::Cleanup.may_start_next());
    fixture.shutdown().await;
}

#[tokio::test]
async fn a_consumer_request_cancels_a_due_cleanup_check() {
    let config = PlayerConfig { cleanup_threshold_ms: 100, ..PlayerConfig::default() };
    let (mut producer, mut consumer) = frame_buffer(config.frame_buffer);
    let control = producer.control();
    producer.begin_track();
    let (events, _receiver) = mpsc::channel(4);
    let mut owner = PlayerOwner::new(Sources::new(), config, producer, control, events);
    owner.track = Some(SourceReference::new("active").into());
    owner.cleanup_last_request = Instant::now() - Duration::from_secs(1);
    assert!(matches!(
        consumer.read(&mut [0; MAX_PACKET_BYTES]),
        Ok(crate::buffer::BufferRead::Empty)
    ));

    owner.cleanup_tick().await.expect("cleanup check");
    assert!(owner.track.is_some());
}

#[test]
fn cleanup_defaults_to_one_minute() {
    assert_eq!(PlayerConfig::default().cleanup_threshold_ms, 60_000);
    assert_eq!(DEFAULT_CLEANUP_THRESHOLD_MS, 60_000);
}
