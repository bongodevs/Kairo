use std::sync::Arc;

use ::lyrics::{LyricsConfig, LyricsService};

use super::update::{set_update_interval, UPDATE_INTERVAL_SECS};
use super::{LavalinkPlayer, SocketContext};
use crate::protocol::message::Message;
use crate::protocol::player::VoiceState;
use crate::utils::locked;

// `tokio::time::interval` panics on a zero period, so a `playerUpdateInterval: 0` in the config
// has to be clamped before it reaches the update task.
#[test]
fn update_interval_is_clamped_and_set_once() {
    set_update_interval(0);
    assert_eq!(UPDATE_INTERVAL_SECS.get().copied(), Some(1));
    // A `OnceLock` keeps the first value, so a second call must be a no-op rather than a panic.
    set_update_interval(9);
    assert_eq!(UPDATE_INTERVAL_SECS.get().copied(), Some(1));
}

fn test_voice(token: &str) -> VoiceState {
    VoiceState {
        token: token.to_string(),
        endpoint: "e".to_string(),
        session_id: "s".to_string(),
        channel_id: Some("c".to_string()),
    }
}

// Needs a runtime: `LavalinkPlayer::new` captures `Handle::current`.
async fn test_player() -> Arc<LavalinkPlayer> {
    test_player_with_channel().await.0
}

async fn test_player_with_channel() -> (
    Arc<LavalinkPlayer>,
    tokio::sync::mpsc::Receiver<Message>,
    // Kept alive: the player holds only a `Weak` context, and dropping this
    // would close the channel the test asserts on.
    Arc<SocketContext>,
) {
    let manager = player::AudioPlayerManager::new();
    let (sender, rx) = tokio::sync::mpsc::channel(crate::session::OUTBOUND_CAPACITY);
    let context = SocketContext::new(
        "test-session".to_string(),
        1,
        manager.clone(),
        None,
        None,
        sender,
    );
    let engine = manager.create_player();
    (
        LavalinkPlayer::new(7, 1, engine, None, None, &context),
        rx,
        context,
    )
}

#[tokio::test]
async fn player_update_is_skipped_while_parked() {
    let (player, _rx, context) = test_player_with_channel().await;
    context.pause();
    player.send_player_update();

    let (sender, _sender_rx) = tokio::sync::mpsc::channel(crate::session::OUTBOUND_CAPACITY);
    let (_epoch, backlog) = context.resume_with(sender);
    assert!(
        backlog.is_empty(),
        "a parked session must not queue stale updates, got {backlog:?}"
    );
}

// A PATCH that carries the voice update the guild already holds must not pay for a second
// handshake. Without a stored connection `voice_reconnect` has to stay true, or the first
// connect after a voice state arrives would be skipped and the guild would stay silent.
#[tokio::test]
async fn voice_reconnect() {
    let player = test_player().await;
    let voice = test_voice("a");
    assert!(player.voice_reconnect(&voice));
    *locked(&player.voice_state) = Some(voice.clone());
    // State matches but nothing is connected, so the connect still has to run.
    assert!(player.voice_reconnect(&voice));
    assert!(player.voice_reconnect(&test_voice("b")));
}

// A 4006 may be retried a few times; the budget resets after a successful handshake and no other
// close code spends an attempt. The actual re-identify needs a live gateway, so this pins the
// policy the event listener applies.
#[tokio::test]
async fn a_4006_reidentify_is_bounded_and_resets() {
    let player = test_player().await;
    assert!(!player.begin_voice_reidentify(4004));
    for _ in 0..3 {
        assert!(player.begin_voice_reidentify(4006));
    }
    assert!(!player.begin_voice_reidentify(4006));
    player.reset_voice_reidentify();
    assert!(player.begin_voice_reidentify(4006));
}

// A PATCH with `endTime: null` must not cancel a marker that was set earlier; the reference's
// `ifPresent` treats null as absent, not as a clear.
#[tokio::test]
async fn a_null_end_time_keeps_the_existing_marker() {
    let player = test_player().await;
    player.set_end_time(Some(1_000));
    let original = *locked(&player.end_marker);
    assert!(original.is_some(), "a positive endTime installs a marker");

    player.set_end_time(None);

    assert_eq!(
        *locked(&player.end_marker),
        original,
        "endTime: null must not clear the marker"
    );
}

// `Late` is fired for a marker installed behind the current position. The reference only acts on
// REACHED/BYPASSED, so a late endTime must not stop playback.
#[test]
fn only_reached_and_bypassed_end_markers_stop_the_track() {
    use super::MarkerState;

    for state in [MarkerState::Reached, MarkerState::Bypassed] {
        assert!(super::end_marker_stops(state), "{state:?} stops the track");
    }
    for state in [
        MarkerState::Late,
        MarkerState::Stopped,
        MarkerState::Ended,
        MarkerState::Removed,
        MarkerState::Overwritten,
    ] {
        assert!(
            !super::end_marker_stops(state),
            "{state:?} must not stop the track"
        );
    }
}

async fn test_player_with_lyrics() -> (Arc<LavalinkPlayer>, Arc<SocketContext>) {
    let manager = player::AudioPlayerManager::new();
    let (sender, _rx) = tokio::sync::mpsc::channel(crate::session::OUTBOUND_CAPACITY);
    let context = SocketContext::new(
        "test-session".to_string(),
        1,
        manager.clone(),
        None,
        None,
        sender,
    );
    let engine = manager.create_player();
    let service = Arc::new(LyricsService::new(&LyricsConfig::default()));
    let player = LavalinkPlayer::new(7, 1, engine, None, Some(service), &context);
    (player, context)
}

// A seek the engine ignores (no seekable track) must not move the lyric anchor either. Before the
// gate, the handle re-anchored and emitted a line the listener never reached.
#[tokio::test]
async fn a_rejected_seek_does_not_reanchor_lyrics() {
    let (player, _context) = test_player_with_lyrics().await;
    let lyrics = player.lyrics.as_ref().expect("lyrics enabled");
    lyrics.subscribe(true);
    let before = lyrics.sync_epoch();

    player.seek(1_000);

    assert_eq!(
        lyrics.sync_epoch(),
        before,
        "a seek the engine ignored must not re-anchor lyrics"
    );
}

// A `TrackException` for a pending gapless successor is attributed through `transition_incoming`,
// so that lock has to survive poison like every other reporting path (G47).
#[tokio::test]
async fn a_poisoned_transition_lock_still_reports_the_exception() {
    use crate::protocol::message::EmittedEvent;
    use crate::protocol::track::{Track, TrackInfo};
    use player::error::FriendlyException;
    use player::player::event::{AudioEvent, AudioEventListener};
    use player::track::AudioTrackInfo;
    use std::panic::AssertUnwindSafe;

    let (player, mut rx, _context) = test_player_with_channel().await;
    let shared = Arc::clone(&player.shared);
    let info = AudioTrackInfo::builder("pending").build();
    *locked(&shared.transition_incoming) = Some(Track::new(
        "encoded".to_string(),
        TrackInfo::from_engine(&info, "test", 0),
    ));

    let poisoned = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = shared.transition_incoming.lock().unwrap();
        panic!("poison the transition lock");
    }));
    assert!(poisoned.is_err(), "the lock must actually be poisoned");

    AudioEventListener::on_event(
        &*shared,
        &AudioEvent::TrackException(info, FriendlyException::fault("boom")),
    );

    match rx
        .try_recv()
        .expect("a poisoned read must not lose the event")
    {
        Message::Event(event) => match *event {
            EmittedEvent::TrackException { track, .. } => {
                assert_eq!(track.info.identifier, "pending");
            }
            other => panic!("expected a TrackException event, got {other:?}"),
        },
        other => panic!("expected an event message, got {other:?}"),
    }
}
