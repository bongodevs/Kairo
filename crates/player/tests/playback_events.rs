use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use super::owner::PlayerOwner;
use super::*;
use crate::audio::MAX_PACKET_BYTES;
use crate::buffer::{BufferRead, FrameBufferConfig, frame_buffer};
use crate::source::{LocalSource, SourceError, SourceReference, Sources};
use crate::test_wav;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[test]
fn exception_severity_distinguishes_load_input_from_playback_faults() {
    assert_eq!(
        TrackException::from_error(&PlayerError::NoMatches).severity,
        ExceptionSeverity::Common
    );
    assert_eq!(
        TrackException::from_error(&PlayerError::WorkerGone).severity,
        ExceptionSeverity::Fault
    );
    assert_eq!(
        TrackException::from_error(&PlayerError::Source(SourceError::load_failed(
            "provider response changed",
            ExceptionSeverity::Suspicious,
        )))
        .severity,
        ExceptionSeverity::Suspicious
    );
    assert_eq!(
        TrackException::from_error(&PlayerError::Decode(
            kairo_audio::decode::DecodeError::NoAudioTrack
        ))
        .severity,
        ExceptionSeverity::Common
    );
    assert_eq!(
        TrackException::from_error(&PlayerError::Decode(kairo_audio::decode::DecodeError::Aac {
            message: "corrupt frame".into()
        }))
        .severity,
        ExceptionSeverity::Suspicious
    );
    assert_eq!(
        TrackException::from_error(&PlayerError::Decode(
            kairo_audio::decode::DecodeError::InvalidAudioFormat
        ))
        .severity,
        ExceptionSeverity::Fault
    );
}

#[tokio::test]
async fn natural_end_is_start_then_finished_after_the_terminator_is_consumed() {
    let (player, mut frames, mut events, guard, track) = player(4_800).await;
    player.play(track.clone(), false).await.expect("play");
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackStart { track: started }) if started == track
    ));

    let mut output = [0u8; MAX_PACKET_BYTES];
    loop {
        match frames.read(&mut output).expect("read") {
            BufferRead::End => break,
            BufferRead::Frame(_) | BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::Closed => panic!("closed before terminator"),
        }
    }
    let Some(PlayerEvent::TrackEnd { track: ended, reason: TrackEndReason::Finished }) =
        events.recv().await
    else {
        panic!("expected finished event");
    };
    assert_eq!(ended, track);
    assert_eq!(ended.position(), crate::decode::Millis::new(80));
    assert!(TrackEndReason::Finished.may_start_next());

    guard.shutdown().await.expect("shutdown");
    cleanup(&track).await;
}

#[tokio::test]
async fn replacement_and_stop_emit_one_end_before_the_next_start() {
    let (player, frames, mut events, guard, first) = player(48_000).await;
    let second = fixture(48_000).await;
    player.play(first.clone(), false).await.expect("first play");
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackStart { track }) if track == first
    ));
    assert!(!player.play(second.clone(), true).await.expect("no replace"));
    assert!(events.try_recv().is_err());

    assert!(player.play(second.clone(), false).await.expect("replace"));
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackEnd { track, reason: TrackEndReason::Replaced })
            if track == first
    ));
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackStart { track }) if track == second
    ));
    player.stop().await.expect("stop");
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackEnd { track, reason: TrackEndReason::Stopped })
            if track == second
    ));
    assert!(!TrackEndReason::Stopped.may_start_next());
    assert!(events.try_recv().is_err());

    drop(frames);
    guard.shutdown().await.expect("shutdown");
    cleanup(&first).await;
    cleanup(&second).await;
}

#[tokio::test]
async fn worker_failure_is_exception_then_load_failed_when_no_frame_was_accepted() {
    let (player, mut frames, mut events, guard, track) = player(48_000).await;
    frames.close();
    player.play(track.clone(), false).await.expect("play");
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackStart { track: started }) if started == track
    ));
    let exception = events.recv().await.expect("exception event");
    let PlayerEvent::TrackException { track: failed, exception } = exception else {
        panic!("expected track exception");
    };
    assert_eq!(failed, track);
    assert_eq!(exception.severity, ExceptionSeverity::Fault);
    assert!(exception.message.is_some());
    assert!(!exception.cause.is_empty());
    assert!(!exception.cause_stack_trace.is_empty());
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackEnd { track: ended, reason: TrackEndReason::LoadFailed })
            if ended == track
    ));

    guard.shutdown().await.expect("shutdown");
    cleanup(&track).await;
}

#[tokio::test]
async fn worker_failure_after_audio_is_exception_then_finished() {
    let (player, mut frames, mut events, guard, track) = player(48_000).await;
    player.play(track.clone(), false).await.expect("play");
    assert!(matches!(events.recv().await, Some(PlayerEvent::TrackStart { .. })));
    wait_until(|| frames.len() == 11).await;
    frames.close();

    assert!(matches!(events.recv().await, Some(PlayerEvent::TrackException { .. })));
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackEnd { track: ended, reason: TrackEndReason::Finished })
            if ended == track
    ));

    guard.shutdown().await.expect("shutdown");
    cleanup(&track).await;
}

#[tokio::test]
async fn stuck_notification_carries_the_configured_threshold_without_ending_the_track() {
    let config = PlayerConfig { track_stuck_threshold_ms: 250, ..PlayerConfig::default() };
    let (mut producer, _consumer) = frame_buffer(config.frame_buffer);
    let control = producer.control();
    control.set_stuck_threshold(config.track_stuck_threshold_ms);
    producer.begin_track();
    let (sender, receiver) = mpsc::channel(4);
    let mut events = PlayerEvents { receiver };
    let mut owner = PlayerOwner::new(Sources::new(), config, producer, control, sender);
    let track = SourceReference::new("stuck-track");
    owner.track = Some(track.clone().into());

    owner.stuck_detected().await;
    assert!(matches!(
        events.recv().await,
        Some(PlayerEvent::TrackStuck { track: stuck, threshold_ms: 250, stack_trace })
            if stuck == track && stack_trace.is_some()
    ));
    assert_eq!(owner.track.as_ref().map(|track| track.reference()), Some(&track));
}

#[tokio::test]
async fn pause_and_resume_emit_once_per_real_transition() {
    let (player, frames, mut events, guard, track) = player(48_000).await;
    player.play(track.clone(), false).await.expect("play");
    assert!(matches!(events.recv().await, Some(PlayerEvent::TrackStart { .. })));

    player.set_paused(true).await.expect("pause");
    let paused = events.recv().await.expect("paused event");
    let PlayerEvent::Paused { track: paused_track } = paused else {
        panic!("expected paused event");
    };
    assert_eq!(paused_track.reference(), &track);

    player.set_paused(true).await.expect("repeat pause");
    assert!(events.try_recv().is_err());

    player.set_paused(false).await.expect("resume");
    let resumed = events.recv().await.expect("resumed event");
    let PlayerEvent::Resumed { track: resumed_track } = resumed else {
        panic!("expected resumed event");
    };
    assert_eq!(resumed_track.reference(), &track);

    player.set_paused(false).await.expect("repeat resume");
    assert!(events.try_recv().is_err());

    drop(frames);
    guard.shutdown().await.expect("shutdown");
    cleanup(&track).await;
}

#[tokio::test]
async fn pause_without_a_track_emits_nothing() {
    let (producer, _consumer) = frame_buffer(PlayerConfig::default().frame_buffer);
    let control = producer.control();
    let (sender, receiver) = mpsc::channel(4);
    let mut events = PlayerEvents { receiver };
    let mut owner =
        PlayerOwner::new(Sources::new(), PlayerConfig::default(), producer, control, sender);

    owner.pause(true).await.expect("pause");
    assert!(owner.paused);
    assert!(events.try_recv().is_err());

    owner.pause(false).await.expect("resume");
    assert!(!owner.paused);
    assert!(events.try_recv().is_err());
}

async fn player(
    frames: u32,
) -> (Player, crate::buffer::FrameConsumer, PlayerEvents, PlayerGuard, SourceReference) {
    let track = fixture(frames).await;
    let mut sources = Sources::new();
    sources.register(LocalSource).expect("register local");
    let config = PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
        ..PlayerConfig::default()
    };
    let (player, frames, events, guard) = spawn_player(sources, config).expect("spawn player");
    (player, frames, events, guard, track)
}

async fn fixture(frames: u32) -> SourceReference {
    let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("kairo-player-events-{}-{id}.wav", std::process::id()));
    tokio::fs::write(&path, test_wav::pcm_wave(48_000, 2, frames)).await.expect("write fixture");
    SourceReference::new(path.to_string_lossy())
}

async fn cleanup(track: &SourceReference) {
    tokio::fs::remove_file(track.identifier()).await.expect("remove fixture");
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..1_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("condition did not become true");
}
