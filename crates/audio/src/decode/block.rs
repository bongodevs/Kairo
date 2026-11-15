use kairo_core::{AudioFormat, Millis};

use crate::opus::OpusPacketInfo;

/// Packet accounting for one decoder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub decoded_packets: u64,
    pub passthrough_packets: u64,
    pub skipped_packets: u64,
}

/// One borrowed native-rate, interleaved f32 block.
#[derive(Debug)]
pub struct PcmBlock<'a> {
    pub samples: &'a [f32],
    pub format: AudioFormat,
    pub frames: usize,
    pub position: Millis,
}

/// One borrowed source Opus packet that already matches the voice output format.
#[derive(Debug)]
pub struct OpusBlock<'a> {
    pub packet: &'a [u8],
    pub info: OpusPacketInfo,
    pub position: Millis,
}

/// The worker either produced native PCM or avoided codec work entirely.
#[derive(Debug)]
pub enum AudioBlock<'a> {
    Pcm(PcmBlock<'a>),
    Opus(OpusBlock<'a>),
}
