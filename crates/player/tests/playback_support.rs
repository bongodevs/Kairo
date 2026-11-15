use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use opus_rs::{Application, OpusEncoder};

use super::*;
use crate::audio::{CHANNELS, FRAME_SAMPLES, MAX_PACKET_BYTES, SAMPLE_RATE};
use crate::buffer::{BufferRead, FrameBufferConfig, FrameConsumer, FrameMeta};
use crate::source::{LocalSource, SourceReference, Sources};
use crate::test_ogg;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

pub(super) struct OpusPlayer {
    pub(super) player: Player,
    pub(super) frames: FrameConsumer,
    pub(super) events: PlayerEvents,
    pub(super) track: SourceReference,
    pub(super) packets: Vec<Vec<u8>>,
    guard: PlayerGuard,
    path: PathBuf,
}

impl OpusPlayer {
    pub(super) async fn new(label: &str, packet_count: usize) -> Self {
        let config = PlayerConfig {
            frame_buffer: FrameBufferConfig::new(200).expect("buffer config"),
            ..PlayerConfig::default()
        };
        Self::with_config(label, packet_count, config).await
    }

    pub(super) async fn with_config(
        label: &str,
        packet_count: usize,
        config: PlayerConfig,
    ) -> Self {
        let packets = opus_packets(packet_count);
        let refs = packets.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("kairo-player-{label}-{}-{id}.ogg", std::process::id()));
        tokio::fs::write(&path, test_ogg::opus_stream(&refs, 2, 0)).await.expect("write fixture");
        let track = SourceReference::new(path.to_string_lossy());
        let mut sources = Sources::new();
        sources.register(LocalSource).expect("register local");
        let (player, frames, events, guard) = spawn_player(sources, config).expect("spawn player");
        Self { player, frames, events, track, packets, guard, path }
    }

    pub(super) async fn shutdown(self) {
        self.guard.shutdown().await.expect("shutdown");
        tokio::fs::remove_file(self.path).await.expect("remove fixture");
    }

    /// Hand the consumer, guard and fixture to a measurement loop. The guard keeps the player's
    /// command channel open, so playback continues after this handle is dropped.
    pub(super) fn split(self) -> (FrameConsumer, PlayerGuard, PathBuf) {
        (self.frames, self.guard, self.path)
    }
}

pub(super) fn opus_packets(count: usize) -> Vec<Vec<u8>> {
    let mut encoder =
        OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio).expect("encoder");
    encoder.bitrate_bps = 99_000;
    encoder.complexity = 5;
    let mut packets = Vec::with_capacity(count);
    for packet_index in 0..count {
        let mut pcm = Vec::with_capacity(FRAME_SAMPLES * CHANNELS);
        for frame in 0..FRAME_SAMPLES {
            let time = (packet_index * FRAME_SAMPLES + frame) as f32 / SAMPLE_RATE as f32;
            let sample = (time * 440.0 * std::f32::consts::TAU).sin() * 0.4;
            pcm.extend_from_slice(&[sample, -sample]);
        }
        let mut output = [0u8; MAX_PACKET_BYTES];
        let length = encoder.encode(&pcm, FRAME_SAMPLES, &mut output).expect("encode packet");
        packets.push(output[..length].to_vec());
    }
    packets
}

pub(super) async fn next_frame(frames: &mut FrameConsumer) -> (FrameMeta, Vec<u8>) {
    let mut output = [0u8; MAX_PACKET_BYTES];
    loop {
        match frames.read(&mut output).expect("read") {
            BufferRead::Frame(meta) => {
                return (meta, output[..usize::from(meta.packet_length)].to_vec());
            }
            BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::End | BufferRead::Closed => panic!("track ended before route transition"),
        }
    }
}
