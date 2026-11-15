use std::time::Duration;

use super::test_support::OpusPlayer;
use crate::buffer::{BufferRead, FrameBufferConfig, FrameConsumer, FrameMeta};
use crate::decode::Millis;
use crate::effects::{CrossfadeCurve, CrossfadeOptions};
use crate::playback::{PlayerConfig, PlayerEvent, PlayerEvents, TrackEndReason};
const CROSSFADE_MS: u64 = 100;

fn config(crossfade: CrossfadeOptions) -> PlayerConfig {
    PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
        crossfade: Some(crossfade),
        ..PlayerConfig::default()
    }
}

fn fade(duration_ms: u64) -> CrossfadeOptions {
    CrossfadeOptions {
        duration_ms,
        manual_duration_ms: 60,
        curve: CrossfadeCurve::SCurve,
        gapless: true,
    }
}

async fn next_event(events: &mut PlayerEvents) -> PlayerEvent {
    tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("event timed out")
        .expect("events closed")
}

async fn drain(frames: &mut FrameConsumer) -> Vec<FrameMeta> {
    let mut output = [0u8; crate::audio::MAX_PACKET_BYTES];
    let mut seen = Vec::new();
    loop {
        match frames.read(&mut output).expect("read frame") {
            BufferRead::Frame(meta) => seen.push(meta),
            BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::End | BufferRead::Closed => return seen,
        }
    }
}

fn position_drops(positions: &[FrameMeta]) -> bool {
    positions.windows(2).any(|pair| pair[1].position < pair[0].position)
}

#[tokio::test]
async fn gapless_hands_the_successor_over_at_the_natural_end() {
    let mut fixture = OpusPlayer::with_config("gapless-a", 30, config(fade(0))).await;
    let second = OpusPlayer::with_config("gapless-b", 15, config(fade(0))).await;

    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(fixture.player.prepare_next(second.track.clone()).await.expect("prepare next"));

    let frames = drain(&mut fixture.frames).await;
    assert!(position_drops(&frames), "successor positions must restart");

    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Gapless, .. }
    ));
    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Finished, .. }
    ));

    fixture.shutdown().await;
    second.shutdown().await;
}

#[tokio::test]
async fn crossfade_overlaps_and_promotes_without_gaps() {
    let mut fixture = OpusPlayer::with_config("xfade-a", 30, config(fade(CROSSFADE_MS))).await;
    let second = OpusPlayer::with_config("xfade-b", 20, config(fade(CROSSFADE_MS))).await;

    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(fixture.player.prepare_next(second.track.clone()).await.expect("prepare next"));

    let frames = drain(&mut fixture.frames).await;
    assert!(position_drops(&frames), "successor positions must restart");
    let positions = frames.iter().map(|frame| frame.position).collect::<Vec<_>>();
    let reset = positions.windows(2).position(|pair| pair[1] < pair[0]).expect("position reset");
    assert!(
        positions[reset] >= Millis::new(500),
        "fade must start near the end of the outgoing track, got {:?}",
        positions[reset]
    );
    assert!(
        positions[reset + 1] <= Millis::new(200),
        "successor must continue from its own timeline, got {:?}",
        positions[reset + 1]
    );

    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Crossfade, .. }
    ));
    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Finished, .. }
    ));

    fixture.shutdown().await;
    second.shutdown().await;
}

#[tokio::test]
async fn replacing_the_track_drops_the_prepared_successor() {
    let mut fixture = OpusPlayer::with_config("replace-a", 30, config(fade(0))).await;
    let second = OpusPlayer::with_config("replace-b", 15, config(fade(0))).await;
    let third = OpusPlayer::with_config("replace-c", 10, config(fade(0))).await;

    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(fixture.player.prepare_next(second.track.clone()).await.expect("prepare next"));
    fixture.player.play(third.track.clone(), false).await.expect("replace");

    let frames = drain(&mut fixture.frames).await;
    assert!(!frames.is_empty());
    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Replaced, .. }
    ));
    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Finished, .. }
    ));

    fixture.shutdown().await;
    second.shutdown().await;
    third.shutdown().await;
}

#[tokio::test]
async fn seeking_clears_a_prepared_successor() {
    let mut fixture = OpusPlayer::with_config("seek-a", 40, config(fade(0))).await;
    let second = OpusPlayer::with_config("seek-b", 15, config(fade(0))).await;

    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(fixture.player.prepare_next(second.track.clone()).await.expect("prepare next"));
    assert!(fixture.player.seek(Millis::new(20)).await.expect("seek"));

    let frames = drain(&mut fixture.frames).await;
    assert!(!frames.is_empty());
    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Finished, .. }
    ));

    fixture.shutdown().await;
    second.shutdown().await;
}

#[tokio::test]
async fn an_explicit_transition_promotes_the_prepared_successor() {
    let mut fixture = OpusPlayer::with_config("manual-a", 30, config(fade(0))).await;
    let second = OpusPlayer::with_config("manual-b", 15, config(fade(0))).await;

    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(fixture.player.prepare_next(second.track.clone()).await.expect("prepare next"));
    assert!(fixture.player.transition_now().await.expect("transition now"));

    let frames = drain(&mut fixture.frames).await;
    assert!(position_drops(&frames));

    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Gapless, .. }
    ));
    assert!(matches!(next_event(&mut fixture.events).await, PlayerEvent::TrackStart { .. }));
    assert!(matches!(
        next_event(&mut fixture.events).await,
        PlayerEvent::TrackEnd { reason: TrackEndReason::Finished, .. }
    ));

    fixture.shutdown().await;
    second.shutdown().await;
}
