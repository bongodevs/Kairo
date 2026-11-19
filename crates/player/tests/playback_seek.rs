use super::test_support::{OpusPlayer, next_frame};
use super::*;
use crate::audio::MAX_PACKET_BYTES;
use crate::buffer::{BufferRead, FrameBufferConfig};
use crate::decode::Millis;
use crate::source::Sources;
#[tokio::test]
async fn seek_without_a_track_is_a_clean_noop() {
    let (player, _frames, _events, guard) =
        spawn_player(Sources::new(), PlayerConfig::default()).expect("spawn player");
    assert!(!player.seek(Millis::new(500)).await.expect("idle seek"));
    guard.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn disabled_seek_ghosting_clears_old_audio_immediately() {
    let config = PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
        use_seek_ghosting: false,
        ..PlayerConfig::default()
    };
    let mut fixture = OpusPlayer::with_config("no-seek-ghost", 160, config).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let _ = next_frame(&mut fixture.frames).await;
    while fixture.frames.len() < config.frame_buffer.capacity() {
        tokio::task::yield_now().await;
    }
    assert!(fixture.player.seek(Millis::new(2_000)).await.expect("seek"));
    // the worker clears the queue before seek returns, but the replacement can already be
    // queued, so the first frame served is the signal, not an empty read
    let (replacement, _) = next_frame(&mut fixture.frames).await;
    assert!(replacement.position >= Millis::new(2_000));
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn seeking_past_end_time_finishes_like_a_reached_marker() {
    let mut fixture = OpusPlayer::new("end-marker-seek", 160).await;
    fixture.player.set_end_time(Some(Millis::new(1_000))).await.expect("end time");
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(matches!(fixture.events.recv().await, Some(PlayerEvent::TrackStart { .. })));
    assert!(fixture.player.seek(Millis::new(2_000)).await.expect("seek"));
    let mut output = [0; MAX_PACKET_BYTES];
    loop {
        match fixture.frames.read(&mut output).expect("read") {
            BufferRead::End => break,
            BufferRead::Frame(_) | BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::Closed => panic!("closed before end marker"),
        }
    }
    assert!(matches!(
        fixture.events.recv().await,
        Some(PlayerEvent::TrackEnd { reason: TrackEndReason::Finished, .. })
    ));
    fixture.shutdown().await;
}
