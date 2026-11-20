#![forbid(unsafe_code)]

mod clock;
mod limit;
mod opus_encode;

pub mod decode;
pub mod filters;
pub mod mix;
pub mod opus;
pub mod pcm;

pub use filters::FilterError;
pub use mix as volume;

/// Discord's decoded and encoded sample rate.
pub const SAMPLE_RATE: u32 = 48_000;
/// Discord voice is always stereo at the player boundary.
pub const CHANNELS: usize = 2;
/// One 20 ms Opus frame at 48 kHz, per channel.
pub const FRAME_SAMPLES: usize = 960;
/// Largest packet that may cross the Discord voice boundary.
pub const MAX_PACKET_BYTES: usize = 1_276;
/// Largest source packet accepted for decode compatibility.
pub const MAX_SOURCE_PACKET_BYTES: usize = 1_568;
/// Expected 20 ms Opus packet storage, matching the current frame-buffer budget.
pub const EXPECTED_PACKET_BYTES: usize = 544;

/// Fixed input side of the streaming sample-rate converter.
pub(crate) const RESAMPLE_CHUNK_FRAMES: usize = 1_024;
/// Bounds converter allocation for malformed or implausible source metadata.
pub(crate) const MIN_SAMPLE_RATE: u32 = 1_000;
pub(crate) const MAX_SAMPLE_RATE: u32 = 768_000;
/// Hard stop if a converter ever fails to make progress while draining.
pub(crate) const MAX_DRAIN_CHUNKS: usize = 16;

#[cfg(test)]
#[path = "../tests/support_mpegts.rs"]
mod test_mpegts;
#[cfg(test)]
#[path = "../tests/support_ogg.rs"]
mod test_ogg;
#[cfg(test)]
#[path = "../tests/support_rss.rs"]
mod test_rss;
#[cfg(test)]
#[path = "../tests/support_wav.rs"]
mod test_wav;
