mod aac;
mod block;
mod codec;
mod decoder;
mod error;
mod input;
mod metadata;
mod mkv;
mod mp4;
mod timeline;

#[cfg(test)]
#[path = "../../tests/decode_decoder.rs"]
mod decoder_tests;
#[cfg(test)]
#[path = "../../tests/decode_opus.rs"]
mod opus_tests;

pub use symphonia::core::packet::Packet;

pub use kairo_core::{AudioFormat, Millis, StreamInfo, TrackMetadata};

pub use self::block::{AudioBlock, DecodeStats, OpusBlock, PcmBlock};
pub use self::decoder::{BlockingDecoder, PacketAudio};
pub use self::error::DecodeError;
