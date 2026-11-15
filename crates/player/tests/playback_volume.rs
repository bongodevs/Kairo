use std::f32::consts::TAU;
use std::sync::atomic::{AtomicU64, Ordering};

use opus_rs::OpusDecoder;

use super::test_support::{next_frame, opus_packets};
use super::*;
use crate::audio::{CHANNELS, FRAME_SAMPLES, MAX_PACKET_BYTES, SAMPLE_RATE};
use crate::buffer::{BufferRead, FrameBufferConfig};
use crate::source::{LocalSource, SourceReference, Sources};
use crate::{test_ogg, test_wav};
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn volume_is_clamped_and_persists_without_a_track() {
    let (player, _frames, _events, guard) =
        spawn_player(Sources::new(), PlayerConfig::default()).expect("spawn player");
    assert_eq!(player.state().await.expect("state").volume, 100);

    player.set_volume(-1).await.expect("mute");
    assert_eq!(player.state().await.expect("muted state").volume, 0);
    player.set_volume(1_001).await.expect("maximum");
    assert_eq!(player.state().await.expect("loud state").volume, 1_000);

    guard.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn live_volume_change_reaches_the_active_encoder() {
    let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("kairo-player-volume-{}-{id}.wav", std::process::id()));
    tokio::fs::write(&path, tone_wave(SAMPLE_RATE * 4)).await.expect("write fixture");
    let reference = SourceReference::new(path.to_string_lossy());
    let mut sources = Sources::new();
    sources.register(LocalSource).expect("register local");
    let config = PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
        ..PlayerConfig::default()
    };
    let (player, mut frames, _events, guard) = spawn_player(sources, config).expect("spawn player");
    player.play(reference.clone(), false).await.expect("play");
    wait_until(|| frames.len() == 11).await;
    player.set_volume(0).await.expect("mute live player");

    let mut packets = Vec::new();
    let mut output = [0u8; MAX_PACKET_BYTES];
    while packets.len() < 90 {
        match frames.read(&mut output).expect("read") {
            BufferRead::Frame(meta) => {
                packets.push(output[..usize::from(meta.packet_length)].to_vec());
            }
            BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::End | BufferRead::Closed => panic!("track ended before volume ramp"),
        }
    }

    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder");
    let mut pcm = vec![0.0; FRAME_SAMPLES * CHANNELS];
    let energies = packets
        .iter()
        .map(|packet| {
            decoder.decode(packet, FRAME_SAMPLES, &mut pcm).expect("decode packet");
            pcm.iter().map(|sample| f64::from(*sample) * f64::from(*sample)).sum::<f64>()
        })
        .collect::<Vec<_>>();
    assert!(energies[0] > 1.0);
    assert!(energies[85..].iter().all(|energy| *energy < 0.000_001));
    assert_eq!(player.state().await.expect("state").volume, 0);

    player.stop().await.expect("stop");
    guard.shutdown().await.expect("shutdown");
    tokio::fs::remove_file(path).await.expect("remove fixture");
}

#[tokio::test]
async fn native_opus_leaves_and_reenters_passthrough_as_volume_changes() {
    let packets = opus_packets(180);
    let packet_refs = packets.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let bytes = test_ogg::opus_stream(&packet_refs, 2, 0);
    let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("kairo-player-volume-{}-{id}.ogg", std::process::id()));
    tokio::fs::write(&path, bytes).await.expect("write fixture");
    let reference = SourceReference::new(path.to_string_lossy());
    let mut sources = Sources::new();
    sources.register(LocalSource).expect("register local");
    let config = PlayerConfig {
        frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
        ..PlayerConfig::default()
    };
    let (player, mut frames, _events, guard) = spawn_player(sources, config).expect("spawn player");
    player.play(reference.clone(), false).await.expect("play");

    let (first, packet) = next_frame(&mut frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, packets[0]);
    player.set_volume(50).await.expect("lower volume");

    let mut left_passthrough = false;
    let mut last_position = first.position.get();
    for _ in 0..40 {
        let (meta, packet) = next_frame(&mut frames).await;
        assert_eq!(meta.position.get(), last_position + 20);
        last_position = meta.position.get();
        let index = usize::try_from(meta.position.get() / 20).expect("packet index");
        if packet != packets[index] {
            left_passthrough = true;
            break;
        }
    }
    assert!(left_passthrough, "volume processing never replaced passthrough");
    player.set_volume(100).await.expect("restore volume");

    let mut resumed_passthrough = false;
    for _ in 0..80 {
        let (meta, packet) = next_frame(&mut frames).await;
        assert_eq!(meta.position.get(), last_position + 20);
        last_position = meta.position.get();
        let index = usize::try_from(meta.position.get() / 20).expect("packet index");
        if packet == packets[index] {
            resumed_passthrough = true;
            break;
        }
    }
    assert!(resumed_passthrough, "unity volume never restored passthrough");

    player.stop().await.expect("stop");
    guard.shutdown().await.expect("shutdown");
    tokio::fs::remove_file(path).await.expect("remove fixture");
}

fn tone_wave(frames: u32) -> Vec<u8> {
    let mut bytes = test_wav::pcm_wave(SAMPLE_RATE, 2, frames);
    for frame in 0..frames {
        let time = frame as f32 / SAMPLE_RATE as f32;
        let sample = ((time * 440.0 * TAU).sin() * 16_384.0) as i16;
        let offset = 44 + usize::try_from(frame).expect("frame offset") * 4;
        bytes[offset..offset + 2].copy_from_slice(&sample.to_le_bytes());
        bytes[offset + 2..offset + 4].copy_from_slice(&(-sample).to_le_bytes());
    }
    bytes
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
