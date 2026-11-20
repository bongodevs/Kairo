//! Frame buffer, worker pipeline, and playback clock.

mod consumer;
mod crossfade;
mod queue;
mod run;
mod worker;

#[cfg(test)]
#[path = "../../tests/buffer_control.rs"]
mod control_tests;
#[cfg(test)]
#[path = "../../tests/buffer_queue.rs"]
mod queue_tests;
#[cfg(test)]
#[path = "../../tests/buffer_scale.rs"]
pub(crate) mod scale;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, AtomicUsize, Ordering};

use thiserror::Error;
use tokio::sync::Notify;

use crate::audio::{CHANNELS, FRAME_SAMPLES, MAX_PACKET_BYTES};
use crate::decode::{Millis, OpusBlock};
use crate::effects::TapeOptions;
use crate::opus::EncodedFrame;
use crate::volume::Volume;
pub use consumer::FrameConsumer;
pub use queue::{FrameProducer, frame_buffer};
#[cfg(test)]
pub(crate) use run::cpu;
pub(crate) use run::{WorkerEnd, WorkerSignal, run_worker};
pub(crate) use worker::{Playback, WorkerState};
pub const MIN_BUFFER_DURATION_MS: u64 = 200;
pub const MAX_BUFFER_DURATION_MS: u64 = 5_000;
pub const DEFAULT_BUFFER_DURATION_MS: u64 = 5_000;
const FRAME_DURATION_MS: u64 = 20;

/// Output encoding format delivered through the frame buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameFormat {
    /// 48 kHz stereo Opus frames (standard Discord voice delivery format).
    #[default]
    Opus,
    /// 48 kHz stereo interleaved 16-bit signed integer PCM frames (3,840 bytes per 20 ms frame).
    PcmS16Le,
}

pub const MAX_PCM_S16_BYTES: usize = FRAME_SAMPLES * CHANNELS * 2;
pub const MAX_FRAME_PAYLOAD_BYTES: usize = MAX_PCM_S16_BYTES;

/// Hard bounds and derived frame capacity for one player's handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBufferConfig {
    duration_ms: u64,
    capacity: usize,
    format: FrameFormat,
}

impl FrameBufferConfig {
    pub fn new(duration_ms: u64) -> Result<Self, BufferError> {
        Self::with_format(duration_ms, FrameFormat::Opus)
    }

    pub fn with_format(duration_ms: u64, format: FrameFormat) -> Result<Self, BufferError> {
        if !(MIN_BUFFER_DURATION_MS..=MAX_BUFFER_DURATION_MS).contains(&duration_ms) {
            return Err(BufferError::InvalidDuration { duration_ms });
        }
        let frames = duration_ms / FRAME_DURATION_MS + 1;
        let capacity =
            usize::try_from(frames).map_err(|_| BufferError::InvalidDuration { duration_ms })?;
        Ok(Self { duration_ms, capacity, format })
    }

    #[must_use]
    pub const fn duration_ms(self) -> u64 {
        self.duration_ms
    }

    #[must_use]
    pub const fn capacity(self) -> usize {
        self.capacity
    }

    #[must_use]
    pub const fn format(self) -> FrameFormat {
        self.format
    }

    #[must_use]
    pub fn maximum_packet_bytes(self) -> usize {
        let max_bytes = match self.format {
            FrameFormat::Opus => MAX_PACKET_BYTES,
            FrameFormat::PcmS16Le => MAX_PCM_S16_BYTES,
        };
        self.capacity.saturating_mul(max_bytes)
    }
}

impl Default for FrameBufferConfig {
    fn default() -> Self {
        Self::new(DEFAULT_BUFFER_DURATION_MS).unwrap_or(Self {
            duration_ms: DEFAULT_BUFFER_DURATION_MS,
            capacity: 251,
            format: FrameFormat::Opus,
        })
    }
}

/// One owned packet copied into the bounded handoff.
pub struct BufferedFrame {
    packet: [u8; MAX_FRAME_PAYLOAD_BYTES],
    length: u16,
    format: FrameFormat,
    position: Millis,
    next_position: Millis,
    pcm_frames: u16,
    volume: u16,
}

impl BufferedFrame {
    pub fn new(packet: &[u8], position: Millis, pcm_frames: usize) -> Result<Self, BufferError> {
        let duration = pcm_frames as u128 * 1_000 / u128::from(crate::audio::SAMPLE_RATE);
        let next_position =
            Millis::new(position.get().saturating_add(u64::try_from(duration).unwrap_or(u64::MAX)));
        Self::with_format_and_volume(
            packet,
            FrameFormat::Opus,
            position,
            next_position,
            pcm_frames,
            Volume::UNITY,
        )
    }

    pub fn new_pcm_s16(
        samples: &[i16],
        position: Millis,
        pcm_frames: usize,
    ) -> Result<Self, BufferError> {
        let byte_len = samples.len() * 2;
        if byte_len > MAX_PCM_S16_BYTES {
            return Err(BufferError::PacketTooLarge { length: byte_len });
        }
        if pcm_frames > FRAME_SAMPLES {
            return Err(BufferError::InvalidPcmFrames { pcm_frames });
        }
        let duration = pcm_frames as u128 * 1_000 / u128::from(crate::audio::SAMPLE_RATE);
        let next_position =
            Millis::new(position.get().saturating_add(u64::try_from(duration).unwrap_or(u64::MAX)));
        let mut storage = [0u8; MAX_FRAME_PAYLOAD_BYTES];
        for (i, &sample) in samples.iter().enumerate() {
            let bytes = sample.to_le_bytes();
            storage[i * 2] = bytes[0];
            storage[i * 2 + 1] = bytes[1];
        }
        Ok(Self {
            packet: storage,
            length: u16::try_from(byte_len)
                .map_err(|_| BufferError::PacketTooLarge { length: byte_len })?,
            format: FrameFormat::PcmS16Le,
            position,
            next_position,
            pcm_frames: u16::try_from(pcm_frames)
                .map_err(|_| BufferError::InvalidPcmFrames { pcm_frames })?,
            volume: Volume::UNITY.level(),
        })
    }

    fn with_format_and_volume(
        packet: &[u8],
        format: FrameFormat,
        position: Millis,
        next_position: Millis,
        pcm_frames: usize,
        volume: Volume,
    ) -> Result<Self, BufferError> {
        let max_len = match format {
            FrameFormat::Opus => MAX_PACKET_BYTES,
            FrameFormat::PcmS16Le => MAX_PCM_S16_BYTES,
        };
        if packet.len() > max_len {
            return Err(BufferError::PacketTooLarge { length: packet.len() });
        }
        if pcm_frames > FRAME_SAMPLES {
            return Err(BufferError::InvalidPcmFrames { pcm_frames });
        }
        let mut storage = [0u8; MAX_FRAME_PAYLOAD_BYTES];
        storage[..packet.len()].copy_from_slice(packet);
        Ok(Self {
            packet: storage,
            length: u16::try_from(packet.len())
                .map_err(|_| BufferError::PacketTooLarge { length: packet.len() })?,
            format,
            position,
            next_position,
            pcm_frames: u16::try_from(pcm_frames)
                .map_err(|_| BufferError::InvalidPcmFrames { pcm_frames })?,
            volume: volume.level(),
        })
    }

    pub fn from_encoded(frame: &EncodedFrame<'_>) -> Result<Self, BufferError> {
        Self::with_format_and_volume(
            frame.packet,
            FrameFormat::Opus,
            frame.position,
            frame.next_position,
            frame.pcm_frames,
            Volume::new(i32::from(frame.volume)),
        )
    }

    pub fn from_passthrough(frame: &OpusBlock<'_>) -> Result<Self, BufferError> {
        Self::with_format_and_volume(
            frame.packet,
            FrameFormat::Opus,
            frame.position,
            Millis::new(frame.position.get().saturating_add(20)),
            frame.info.total_samples,
            Volume::UNITY,
        )
    }

    #[must_use]
    pub const fn format(&self) -> FrameFormat {
        self.format
    }

    #[must_use]
    pub fn packet(&self) -> &[u8] {
        &self.packet[..usize::from(self.length)]
    }

    #[must_use]
    pub const fn meta(&self) -> FrameMeta {
        FrameMeta {
            position: self.position,
            pcm_frames: self.pcm_frames,
            packet_length: self.length,
        }
    }

    pub(crate) fn volume(&self) -> Volume {
        Volume::new(i32::from(self.volume))
    }

    pub(crate) const fn next_position(&self) -> Millis {
        self.next_position
    }
}

impl std::fmt::Debug for BufferedFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BufferedFrame")
            .field("packet_length", &self.length)
            .field("position", &self.position)
            .field("next_position", &self.next_position)
            .field("pcm_frames", &self.pcm_frames)
            .field("volume", &self.volume)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    pub position: Millis,
    pub pcm_frames: u16,
    pub packet_length: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferRead {
    Frame(FrameMeta),
    Empty,
    End,
    Closed,
}

pub struct FrameSend {
    frame: Option<BufferedFrame>,
}

impl FrameSend {
    pub(crate) fn sent() -> Self {
        Self { frame: None }
    }

    pub(crate) fn woken(frame: BufferedFrame) -> Self {
        Self { frame: Some(frame) }
    }

    #[must_use]
    pub fn into_woken(self) -> Option<BufferedFrame> {
        self.frame
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndSend {
    Sent,
    Woken,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BufferError {
    #[error("frame buffer duration {duration_ms} ms is outside 200..=5000")]
    InvalidDuration { duration_ms: u64 },
    #[error("Opus packet is {length} bytes, above the 1,276-byte bound")]
    PacketTooLarge { length: usize },
    #[error("PCM frame count {pcm_frames} exceeds one 20 ms frame")]
    InvalidPcmFrames { pcm_frames: usize },
    #[error("output has {available} bytes, frame needs {required}")]
    OutputTooSmall { required: usize, available: usize },
    #[error("frame consumer is closed")]
    Closed,
}

pub(super) const NO_POSITION: u64 = u64::MAX;

pub(super) struct State {
    pub(super) active: AtomicU64,
    pub(super) depth: AtomicUsize,
    pub(super) received: AtomicBool,
    pub(super) paused: AtomicBool,
    pub(super) playing: AtomicBool,
    pub(super) discontinuity: AtomicBool,
    pub(super) last_position: AtomicU64,
    pub(super) output_position: AtomicU64,
    pub(super) next_output_position: AtomicU64,
    pub(super) output_volume: AtomicU16,
    pub(super) end_time: AtomicU64,
    pub(super) end_sequence: AtomicU64,
    pub(super) end_notify: Notify,
    pub(super) track_epoch: AtomicU64,
    pub(super) worker_progress: AtomicU64,
    pub(super) stuck_reset: AtomicU64,
    pub(super) stuck_threshold_ms: AtomicU64,
    pub(super) stuck_sequence: AtomicU64,
    pub(super) stuck_notify: Notify,
    pub(super) tape_enabled: AtomicBool,
    pub(super) tape_duration_ms: AtomicU64,
    pub(super) tape_curve: AtomicU8,
    pub(super) tape_epoch: AtomicU64,
    pub(super) markers: Arc<crate::track::marker::TrackMarkerTracker>,
    requested: AtomicBool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            active: AtomicU64::new(0),
            depth: AtomicUsize::new(0),
            received: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            discontinuity: AtomicBool::new(false),
            last_position: AtomicU64::new(NO_POSITION),
            output_position: AtomicU64::new(NO_POSITION),
            next_output_position: AtomicU64::new(NO_POSITION),
            output_volume: AtomicU16::new(crate::volume::DEFAULT_VOLUME),
            end_time: AtomicU64::new(NO_POSITION),
            end_sequence: AtomicU64::new(0),
            end_notify: Notify::new(),
            track_epoch: AtomicU64::new(0),
            worker_progress: AtomicU64::new(0),
            stuck_reset: AtomicU64::new(0),
            stuck_threshold_ms: AtomicU64::new(10_000),
            stuck_sequence: AtomicU64::new(0),
            stuck_notify: Notify::new(),
            tape_enabled: AtomicBool::new(false),
            tape_duration_ms: AtomicU64::new(0),
            tape_curve: AtomicU8::new(0),
            tape_epoch: AtomicU64::new(0),
            markers: Arc::new(crate::track::marker::TrackMarkerTracker::new()),
            requested: AtomicBool::new(false),
        }
    }
}

impl State {
    pub(super) fn note_request(&self) {
        self.requested.store(true, Ordering::Relaxed);
    }

    pub(super) fn take_request(&self) -> bool {
        self.requested.swap(false, Ordering::Relaxed)
    }

    pub(super) fn set_tape(&self, options: Option<TapeOptions>) {
        match options {
            Some(options) => {
                self.tape_duration_ms.store(options.duration_ms, Ordering::Relaxed);
                self.tape_curve.store(options.curve.code(), Ordering::Relaxed);
                self.tape_enabled.store(true, Ordering::Release);
            }
            None => self.tape_enabled.store(false, Ordering::Release),
        }
        self.tape_epoch.fetch_add(1, Ordering::AcqRel);
    }
}

pub(super) struct Queued {
    pub(super) generation: u64,
    pub(super) frame: Option<BufferedFrame>,
}

impl Queued {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
}

pub(super) fn decrement(value: &AtomicUsize) {
    let mut depth = value.load(Ordering::Relaxed);
    loop {
        match value.compare_exchange_weak(
            depth,
            depth.saturating_sub(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(current) => depth = current,
        }
    }
}

#[derive(Clone)]
pub(crate) struct FrameControl {
    state: Arc<State>,
}

impl FrameControl {
    pub(super) fn new(state: Arc<State>) -> Self {
        Self { state }
    }

    pub(crate) fn set_end_time(&self, end_time: Option<Millis>) {
        self.state.end_time.store(end_time.map_or(NO_POSITION, Millis::get), Ordering::Release);
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        self.state.paused.store(paused, Ordering::Release);
        self.reset_stuck_clock();
    }

    pub(crate) fn set_tape(&self, options: Option<TapeOptions>) {
        self.state.set_tape(options);
    }

    pub(crate) fn end_track(&self) {
        self.state.playing.store(false, Ordering::Release);
        self.state.discontinuity.store(false, Ordering::Release);
        self.reset_stuck_clock();
    }

    pub(crate) fn output_state(&self) -> Option<(Millis, Volume)> {
        let position = self.state.next_output_position.load(Ordering::Acquire);
        if position == NO_POSITION {
            return None;
        }
        let volume = self.state.output_volume.load(Ordering::Acquire);
        Some((Millis::new(position), Volume::new(i32::from(volume))))
    }

    pub(crate) fn position(&self) -> Option<Millis> {
        let position = self.state.output_position.load(Ordering::Acquire);
        (position != NO_POSITION).then_some(Millis::new(position))
    }

    pub(crate) fn take_request(&self) -> bool {
        self.state.take_request()
    }

    pub(crate) fn has_received_frames(&self) -> bool {
        self.state.received.load(Ordering::Acquire)
    }

    pub(crate) fn end_sequence(&self) -> u64 {
        self.state.end_sequence.load(Ordering::Acquire)
    }

    pub(crate) async fn wait_for_end(&self, seen: u64) -> u64 {
        wait_for_change(&self.state.end_sequence, &self.state.end_notify, seen).await
    }

    pub(crate) fn set_stuck_threshold(&self, threshold_ms: u64) {
        self.state.stuck_threshold_ms.store(threshold_ms, Ordering::Release);
    }

    pub(crate) fn stuck_sequence(&self) -> u64 {
        self.state.stuck_sequence.load(Ordering::Acquire)
    }

    pub(crate) fn stuck_threshold_ms(&self) -> u64 {
        self.state.stuck_threshold_ms.load(Ordering::Acquire)
    }

    pub(crate) async fn wait_for_stuck(&self, seen: u64) -> u64 {
        wait_for_change(&self.state.stuck_sequence, &self.state.stuck_notify, seen).await
    }

    #[must_use]
    pub fn markers(&self) -> Arc<crate::track::marker::TrackMarkerTracker> {
        Arc::clone(&self.state.markers)
    }

    fn reset_stuck_clock(&self) {
        self.state.stuck_reset.fetch_add(1, Ordering::AcqRel);
    }
}

async fn wait_for_change(
    value: &std::sync::atomic::AtomicU64,
    notify: &tokio::sync::Notify,
    seen: u64,
) -> u64 {
    loop {
        let notified = notify.notified();
        let current = value.load(Ordering::Acquire);
        if current != seen {
            return current;
        }
        notified.await;
    }
}
