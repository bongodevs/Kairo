#![forbid(unsafe_code)]

mod config;
mod error;
mod event;
pub mod manager;
mod pipeline;
mod player;
pub mod track;

pub mod buffer;
pub mod playback;
pub use kairo_audio as audio;

pub use kairo_audio::decode;
pub use kairo_audio::filters;
pub use kairo_audio::opus;
pub use kairo_audio::pcm;
pub use kairo_audio::volume;
pub use kairo_core::ExceptionSeverity;
pub mod source;
pub mod source_probe;
pub mod effects {
    pub use kairo_audio::mix::{
        CrossfadeCurve, CrossfadeOptions, TapeCurve, TapeEffect, TapeOptions,
    };
}

#[cfg(test)]
#[path = "../tests/source_support_audio_probe.rs"]
mod test_audio_probe;
#[cfg(test)]
#[path = "../tests/support_mpegts.rs"]
mod test_mpegts;
#[cfg(test)]
#[path = "../tests/support_ogg.rs"]
mod test_ogg;
#[cfg(test)]
#[path = "../tests/support_wav.rs"]
mod test_wav;
