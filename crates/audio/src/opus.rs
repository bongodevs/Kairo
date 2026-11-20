#[cfg(test)]
#[path = "../tests/opus_packet.rs"]
mod packet_tests;

use thiserror::Error;

use kairo_core::Millis;

use crate::decode::AudioFormat;
pub use crate::opus_encode::OpusFrameEncoder;
pub use crate::{CHANNELS, FRAME_SAMPLES, MAX_PACKET_BYTES, MAX_SOURCE_PACKET_BYTES, SAMPLE_RATE};
const MAX_FRAMES: usize = 48;
const MAX_SAMPLES: usize = 5_760;

/// Failure while assembling or encoding a 20 ms Opus frame.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpusError {
    #[error("bitrate must be between 500 and 3,000,000 bit/s")]
    InvalidBitrate,

    #[error("encoder complexity must be between 0 and 10")]
    InvalidComplexity,

    #[error("expected packet loss must be between 0 and 100 percent")]
    InvalidPacketLoss,

    #[error("Opus input must be 48 kHz stereo, got {format:?}")]
    InvalidFormat { format: AudioFormat },

    #[error("PCM block contains {actual} samples, expected {expected}")]
    InvalidBlock { expected: usize, actual: usize },

    #[error("Opus encoder initialization failed: {message}")]
    Initialize { message: &'static str },

    #[error("Opus frame encoding failed: {message}")]
    Encode { message: &'static str },

    #[error("PCM was submitted after the encoder had finished")]
    Finished,

    #[error(transparent)]
    Filter(#[from] crate::filters::FilterError),
}

/// Structural information needed to pace or decode an Opus packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusPacketInfo {
    pub frame_count: usize,
    pub samples_per_frame: usize,
    pub total_samples: usize,
    pub channels: usize,
}

impl OpusPacketInfo {
    #[must_use]
    pub fn is_voice_passthrough_frame(self) -> bool {
        self.channels == CHANNELS && self.total_samples == FRAME_SAMPLES
    }

    #[must_use]
    pub fn can_passthrough_voice(self, packet_len: usize) -> bool {
        packet_len <= MAX_PACKET_BYTES && self.is_voice_passthrough_frame()
    }
}

/// Malformed or unsupported RFC 6716 packet structure.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PacketError {
    #[error("Opus packet is empty")]
    Empty,
    #[error("source Opus packet exceeds the 1,568-byte decode bound")]
    Oversized,
    #[error("Opus packet has an invalid frame count")]
    FrameCount,
    #[error("Opus packet contains an empty frame")]
    EmptyFrame,
    #[error("Opus packet length table exceeds its payload")]
    FrameLength,
    #[error("Opus packet padding exceeds its payload")]
    Padding,
    #[error("Opus packet duration exceeds 120 ms")]
    Duration,
}

/// Validate an Opus packet and derive its 48 kHz duration/channel shape.
pub fn inspect_packet(packet: &[u8]) -> Result<OpusPacketInfo, PacketError> {
    if packet.is_empty() {
        return Err(PacketError::Empty);
    }
    if packet.len() > MAX_SOURCE_PACKET_BYTES {
        return Err(PacketError::Oversized);
    }

    let toc = packet[0];
    let samples_per_frame = samples_per_frame(toc);
    let frame_count = validate_layout(packet)?;
    let total_samples = samples_per_frame.checked_mul(frame_count).ok_or(PacketError::Duration)?;
    if total_samples > MAX_SAMPLES {
        return Err(PacketError::Duration);
    }

    Ok(OpusPacketInfo {
        frame_count,
        samples_per_frame,
        total_samples,
        channels: if toc & 0x04 == 0 { 1 } else { 2 },
    })
}

fn samples_per_frame(toc: u8) -> usize {
    let config = toc >> 3;
    match config {
        0..=11 => [480, 960, 1_920, 2_880][usize::from(config & 0x03)],
        12..=15 => [480, 960][usize::from(config & 0x01)],
        _ => [120, 240, 480, 960][usize::from(config & 0x03)],
    }
}

fn validate_layout(packet: &[u8]) -> Result<usize, PacketError> {
    match packet[0] & 0x03 {
        0 => Ok(1),
        1 => {
            let payload = packet.len() - 1;
            if payload == 0 || !payload.is_multiple_of(2) {
                return Err(PacketError::FrameLength);
            }
            Ok(2)
        }
        2 => validate_two_vbr(packet),
        3 => validate_arbitrary(packet),
        _ => unreachable!("two-bit packet code"),
    }
}

fn validate_two_vbr(packet: &[u8]) -> Result<usize, PacketError> {
    let (first, header) = frame_length(&packet[1..])?;
    let second = packet.len().saturating_sub(1 + header + first);
    if first == 0 || second == 0 || 1 + header + first > packet.len() {
        return Err(PacketError::FrameLength);
    }
    Ok(2)
}

fn validate_arbitrary(packet: &[u8]) -> Result<usize, PacketError> {
    let Some(&control) = packet.get(1) else {
        return Err(PacketError::FrameCount);
    };
    let frames = usize::from(control & 0x3f);
    if !(1..=MAX_FRAMES).contains(&frames) {
        return Err(PacketError::FrameCount);
    }

    let mut cursor = 2;
    let padding = if control & 0x40 == 0 { 0 } else { parse_padding(packet, &mut cursor)? };
    let payload_end = packet.len().checked_sub(padding).ok_or(PacketError::Padding)?;
    if cursor >= payload_end {
        return Err(PacketError::EmptyFrame);
    }

    if control & 0x80 == 0 {
        let payload = payload_end - cursor;
        if payload == 0 || !payload.is_multiple_of(frames) {
            return Err(PacketError::FrameLength);
        }
        return Ok(frames);
    }

    let mut described = 0usize;
    for _ in 1..frames {
        let (length, bytes) = frame_length(&packet[cursor..payload_end])?;
        if length == 0 {
            return Err(PacketError::EmptyFrame);
        }
        cursor = cursor.checked_add(bytes).ok_or(PacketError::FrameLength)?;
        described = described.checked_add(length).ok_or(PacketError::FrameLength)?;
        if cursor + described >= payload_end {
            return Err(PacketError::FrameLength);
        }
    }
    Ok(frames)
}

fn parse_padding(packet: &[u8], cursor: &mut usize) -> Result<usize, PacketError> {
    let mut padding = 0usize;
    loop {
        let byte = *packet.get(*cursor).ok_or(PacketError::Padding)?;
        *cursor += 1;
        padding = padding
            .checked_add(if byte == 255 { 254 } else { usize::from(byte) })
            .ok_or(PacketError::Padding)?;
        if byte != 255 {
            return Ok(padding);
        }
    }
}

fn frame_length(data: &[u8]) -> Result<(usize, usize), PacketError> {
    let first = usize::from(*data.first().ok_or(PacketError::FrameLength)?);
    if first < 252 {
        return Ok((first, 1));
    }
    let second = usize::from(*data.get(1).ok_or(PacketError::FrameLength)?);
    Ok((first + 4 * second, 2))
}

/// Settings applied once when a player's encoder is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusEncoderConfig {
    pub bitrate_bps: i32,
    pub complexity: i32,
    pub use_cbr: bool,
    pub use_inband_fec: bool,
    pub packet_loss_percent: i32,
}

// Listening found a clear gain over 96 kbps and no meaningful gain at the next step.
const MUSIC_BITRATE_BPS: i32 = 128_000;

impl Default for OpusEncoderConfig {
    fn default() -> Self {
        Self {
            bitrate_bps: MUSIC_BITRATE_BPS,
            complexity: 10,
            use_cbr: false,
            use_inband_fec: false,
            packet_loss_percent: 0,
        }
    }
}

impl OpusEncoderConfig {
    pub fn validate(self) -> Result<Self, OpusError> {
        if !(500..=3_000_000).contains(&self.bitrate_bps) {
            return Err(OpusError::InvalidBitrate);
        }
        if !(0..=10).contains(&self.complexity) {
            return Err(OpusError::InvalidComplexity);
        }
        if !(0..=100).contains(&self.packet_loss_percent) {
            return Err(OpusError::InvalidPacketLoss);
        }
        Ok(self)
    }

    pub(crate) fn apply_to(self, encoder: &mut opus_rs::OpusEncoder) {
        encoder.bitrate_bps = self.bitrate_bps;
        encoder.complexity = self.complexity;
        encoder.use_cbr = self.use_cbr;
        encoder.use_inband_fec = self.use_inband_fec;
        encoder.packet_loss_perc = self.packet_loss_percent;
    }
}

/// One borrowed Opus packet. The next encode call reuses its storage.
#[derive(Debug)]
pub struct EncodedFrame<'a> {
    pub packet: &'a [u8],
    pub position: Millis,
    pub next_position: Millis,
    /// Real PCM frames before final zero-padding; normally 960.
    pub pcm_frames: usize,
    /// Player volume applied before encoding this frame.
    pub volume: u16,
}

impl EncodedFrame<'_> {
    #[must_use]
    pub fn is_final_partial(&self) -> bool {
        self.pcm_frames < FRAME_SAMPLES
    }
}

/// Encoder accounting for one player.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpusStats {
    pub frames: u64,
    pub bytes: u64,
}

/// Processing state consulted for every source Opus packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusRoute {
    pub volume: u16,
    pub filters_active: bool,
}

impl Default for OpusRoute {
    fn default() -> Self {
        Self { volume: 100, filters_active: false }
    }
}

impl OpusRoute {
    #[must_use]
    pub fn allows_passthrough(self) -> bool {
        self.volume == 100 && !self.filters_active
    }

    #[must_use]
    pub const fn decode() -> Self {
        Self { volume: 100, filters_active: true }
    }
}

const _: () = assert!(MAX_PACKET_BYTES <= u16::MAX as usize);
