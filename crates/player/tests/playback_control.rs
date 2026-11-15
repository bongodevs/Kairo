use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::audio::MAX_PACKET_BYTES;
use crate::buffer::{BufferRead, FrameBufferConfig};
use crate::decode::Millis;
use crate::source::{LocalSource, SourceReference, Sources};
use crate::test_wav;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

async fn player(
    frames: u32,
    paused: bool,
) -> (Player, crate::buffer::FrameConsumer, PlayerGuard, SourceReference) {
    let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("kairo-player-control-{}-{id}.wav", std::process::id()));
    tokio::fs::write(&path, test_wav::pcm_wave(48_000, 2, frames)).await.expect("write fixture");
    let mut sources = Sources::new();
    sources.register(LocalSource).expect("register local");
    let config = PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
        ..PlayerConfig::default()
    };
    let (player, frames, _events, guard) = spawn_player(sources, config).expect("spawn player");
    if paused {
        player.set_paused(true).await.expect("pause idle player");
    }
    (player, frames, guard, SourceReference::new(path.to_string_lossy()))
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

async fn cleanup(reference: &SourceReference) {
    tokio::fs::remove_file(reference.identifier()).await.expect("remove fixture");
}

#[tokio::test]
async fn play_runs_the_complete_pipeline_and_natural_end_becomes_idle() {
    let (player, mut frames, guard, reference) = player(4_800, false).await;
    assert!(player.play(reference.clone(), false).await.expect("play"));

    let mut output = [0u8; MAX_PACKET_BYTES];
    let mut packet_count = 0;
    loop {
        match frames.read(&mut output).expect("read") {
            BufferRead::Frame(_) => packet_count += 1,
            BufferRead::End => break,
            BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::Closed => panic!("buffer closed before end"),
        }
    }
    assert_eq!(packet_count, 5);
    assert_eq!(player.state().await.expect("state").track, None);

    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}

#[tokio::test]
async fn no_replace_keeps_the_active_track() {
    let (player, frames, guard, reference) = player(48_000 * 5, false).await;
    assert!(player.play(reference.clone(), false).await.expect("play"));
    wait_until(|| frames.len() == 11).await;
    assert!(!player.play(reference.clone(), true).await.expect("no replace"));
    assert_eq!(
        player.state().await.expect("state").track.as_ref().map(|track| track.reference()),
        Some(&reference)
    );

    player.stop().await.expect("stop");
    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}

#[tokio::test]
async fn pause_releases_the_worker_and_holds_queued_audio_until_resume() {
    let (player, mut frames, guard, reference) = player(48_000 * 5, false).await;
    player.play(reference.clone(), false).await.expect("play");
    wait_until(|| frames.len() == 11).await;

    player.set_paused(true).await.expect("pause");
    let state = player.state().await.expect("state");
    assert!(state.paused);
    assert_eq!(state.track.as_ref().map(|track| track.reference()), Some(&reference));
    assert_eq!(frames.read(&mut [0; MAX_PACKET_BYTES]).expect("paused read"), BufferRead::Empty);

    player.set_paused(false).await.expect("resume");
    let mut output = [0u8; MAX_PACKET_BYTES];
    wait_until(|| matches!(frames.read(&mut output), Ok(BufferRead::Frame(_)))).await;

    player.stop().await.expect("stop");
    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}

#[tokio::test]
async fn stop_invalidates_buffered_audio_and_is_idempotent() {
    let (player, mut frames, guard, reference) = player(48_000 * 5, false).await;
    player.play(reference.clone(), false).await.expect("play");
    wait_until(|| frames.len() == 11).await;
    player.stop().await.expect("stop");
    player.stop().await.expect("second stop");
    assert_eq!(player.state().await.expect("state").track, None);
    assert_eq!(frames.read(&mut [0; MAX_PACKET_BYTES]).expect("cleared read"), BufferRead::Empty);

    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}

#[tokio::test]
async fn a_track_started_while_paused_does_no_work_until_resume() {
    let (player, mut frames, guard, reference) = player(4_800, true).await;
    player.play(reference.clone(), false).await.expect("play");
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(frames.len(), 0);
    assert_eq!(frames.read(&mut [0; MAX_PACKET_BYTES]).expect("paused read"), BufferRead::Empty);
    player.set_paused(false).await.expect("resume");
    wait_until(|| !frames.is_empty()).await;

    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}

#[tokio::test]
async fn a_failed_load_leaves_the_player_reusable_and_shutdown_is_joined() {
    let mut sources = Sources::new();
    sources.register(LocalSource).expect("register local");
    let (player, _frames, _events, guard) =
        spawn_player(sources, PlayerConfig::default()).expect("spawn player");
    let error = player
        .play(SourceReference::new("/definitely/not/a/track.wav"), false)
        .await
        .expect_err("missing source");
    assert!(matches!(error, PlayerError::NoMatches));
    assert_eq!(player.state().await.expect("state"), PlayerState::default());
    guard.shutdown().await.expect("shutdown");
    assert!(matches!(player.state().await, Err(PlayerError::Stopped)));
}

#[tokio::test]
async fn a_thousand_idle_player_tasks_all_join() {
    let mut guards = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let (player, frames, _events, guard) =
            spawn_player(Sources::new(), PlayerConfig::default()).expect("spawn player");
        drop(player);
        drop(frames);
        guards.push(guard);
    }
    for guard in guards {
        guard.shutdown().await.expect("joined shutdown");
    }
}

#[tokio::test]
async fn seek_keeps_old_audio_until_the_first_replacement_frame() {
    let (player, mut frames, guard, reference) = player(48_000 * 5, false).await;
    player.play(reference.clone(), false).await.expect("play");
    wait_until(|| frames.len() == 11).await;
    assert!(player.seek(Millis::new(2_000)).await.expect("seek"));

    let mut output = [0u8; MAX_PACKET_BYTES];
    let BufferRead::Frame(old) = frames.read(&mut output).expect("old frame") else {
        panic!("ghost buffer should provide old audio");
    };
    assert!(old.position.get() < 2_000);
    let mut replacement = None;
    wait_until(|| {
        if let Ok(BufferRead::Frame(meta)) = frames.read(&mut output)
            && meta.position.get() >= 2_000
        {
            replacement = Some(meta);
            return true;
        }
        false
    })
    .await;
    assert!(replacement.is_some());

    player.stop().await.expect("stop");
    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}

#[tokio::test]
async fn end_time_stops_before_the_frame_start_at_the_limit() {
    let (player, mut frames, guard, reference) = player(48_000, false).await;
    player.set_end_time(Some(Millis::new(100))).await.expect("end time");
    player.play(reference.clone(), false).await.expect("play");

    let mut output = [0u8; MAX_PACKET_BYTES];
    let mut positions = Vec::new();
    loop {
        match frames.read(&mut output).expect("read") {
            BufferRead::Frame(meta) => positions.push(meta.position.get()),
            BufferRead::End => break,
            BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::Closed => panic!("closed before end time"),
        }
    }
    assert_eq!(positions, [0, 20, 40, 60, 80]);
    assert_eq!(player.state().await.expect("state").track, None);

    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}

#[tokio::test]
async fn end_time_also_cuts_frames_that_were_already_buffered() {
    let (player, mut frames, guard, reference) = player(48_000, false).await;
    player.play(reference.clone(), false).await.expect("play");
    wait_until(|| frames.len() == 11).await;
    player.set_end_time(Some(Millis::new(100))).await.expect("end time");

    let mut output = [0u8; MAX_PACKET_BYTES];
    let mut positions = Vec::new();
    loop {
        match frames.read(&mut output).expect("read") {
            BufferRead::Frame(meta) => positions.push(meta.position.get()),
            BufferRead::End => break,
            BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::Closed => panic!("closed before end time"),
        }
    }
    assert_eq!(positions, [0, 20, 40, 60, 80]);

    guard.shutdown().await.expect("shutdown");
    cleanup(&reference).await;
}
