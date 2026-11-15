mod biquad;
mod channel_mix;
mod cutoff_low_pass;
mod distortion;
mod echo;
mod equalizer;
mod high_pass;
mod karaoke;
mod low_pass;
mod normalization;
mod rotation;
mod timescale;
mod tremolo;
mod vibrato;
mod volume;

#[cfg(test)]
#[path = "../../tests/filters_core.rs"]
mod tests;
#[cfg(test)]
#[path = "../../tests/filters_volume.rs"]
mod volume_tests;

pub use self::channel_mix::ChannelMix;
pub use self::cutoff_low_pass::CutoffLowPass;
pub use self::distortion::Distortion;
pub use self::echo::Echo;
pub use self::equalizer::Equalizer;
pub use self::high_pass::HighPass;
pub use self::karaoke::Karaoke;
pub use self::low_pass::LowPass;
pub use self::normalization::Normalization;
pub use self::rotation::Rotation;
pub use self::timescale::Timescale;
pub use self::tremolo::Tremolo;
pub use self::vibrato::Vibrato;
pub use self::volume::FilterVolume;
pub use kairo_core::filters::{
    BAND_COUNT, ChannelMixConfig, DistortionConfig, EchoConfig, EqualizerConfig, FilterError,
    Filters, KaraokeConfig, LowPassConfig, MAX_ECHO_SECONDS, NormalizationConfig, PassConfig,
    RotationConfig, TimescaleConfig, TremoloConfig, VibratoConfig,
};
