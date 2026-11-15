use serde::{Deserialize, Serialize};

use player::filters::{
    ChannelMixConfig, DistortionConfig, KaraokeConfig, LowPassConfig, RotationConfig,
    TimescaleConfig, TremoloConfig, VibratoConfig,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub(super) struct Band {
    pub(super) band: usize,
    #[serde(default = "one_f32")]
    pub(super) gain: f32,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Karaoke {
    #[serde(default = "one_f32")]
    level: f32,
    #[serde(default = "one_f32")]
    mono_level: f32,
    #[serde(default = "karaoke_band")]
    filter_band: f32,
    #[serde(default = "karaoke_width")]
    filter_width: f32,
}

impl From<Karaoke> for KaraokeConfig {
    fn from(value: Karaoke) -> Self {
        Self {
            level: value.level,
            mono_level: value.mono_level,
            filter_band: value.filter_band,
            filter_width: value.filter_width,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub(super) struct Timescale {
    #[serde(default = "one_f64")]
    speed: f64,
    #[serde(default = "one_f64")]
    pitch: f64,
    #[serde(default = "one_f64")]
    rate: f64,
}

impl From<Timescale> for TimescaleConfig {
    fn from(value: Timescale) -> Self {
        Self { speed: value.speed, pitch: value.pitch, rate: value.rate }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub(super) struct Modulation {
    #[serde(default = "modulation_frequency")]
    frequency: f32,
    #[serde(default = "modulation_depth")]
    depth: f32,
}

impl From<Modulation> for TremoloConfig {
    fn from(value: Modulation) -> Self {
        Self { frequency: value.frequency, depth: value.depth }
    }
}

impl From<Modulation> for VibratoConfig {
    fn from(value: Modulation) -> Self {
        Self { frequency: value.frequency, depth: value.depth }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Distortion {
    #[serde(default)]
    sin_offset: f32,
    #[serde(default = "one_f32")]
    sin_scale: f32,
    #[serde(default)]
    cos_offset: f32,
    #[serde(default = "one_f32")]
    cos_scale: f32,
    #[serde(default)]
    tan_offset: f32,
    #[serde(default = "one_f32")]
    tan_scale: f32,
    #[serde(default)]
    offset: f32,
    #[serde(default = "one_f32")]
    scale: f32,
}

impl From<Distortion> for DistortionConfig {
    fn from(value: Distortion) -> Self {
        Self {
            sin_offset: value.sin_offset,
            sin_scale: value.sin_scale,
            cos_offset: value.cos_offset,
            cos_scale: value.cos_scale,
            tan_offset: value.tan_offset,
            tan_scale: value.tan_scale,
            offset: value.offset,
            scale: value.scale,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Rotation {
    #[serde(default)]
    rotation_hz: f64,
}

impl From<Rotation> for RotationConfig {
    fn from(value: Rotation) -> Self {
        Self { rotation_hz: value.rotation_hz }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChannelMix {
    #[serde(default = "one_f32")]
    left_to_left: f32,
    #[serde(default)]
    left_to_right: f32,
    #[serde(default)]
    right_to_left: f32,
    #[serde(default = "one_f32")]
    right_to_right: f32,
}

impl From<ChannelMix> for ChannelMixConfig {
    fn from(value: ChannelMix) -> Self {
        Self {
            left_to_left: value.left_to_left,
            left_to_right: value.left_to_right,
            right_to_left: value.right_to_left,
            right_to_right: value.right_to_right,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub(super) struct LowPass {
    #[serde(default = "low_pass_smoothing")]
    smoothing: f32,
}

impl From<LowPass> for LowPassConfig {
    fn from(value: LowPass) -> Self {
        Self { smoothing: value.smoothing }
    }
}

const fn one_f32() -> f32 {
    1.0
}

const fn one_f64() -> f64 {
    1.0
}

const fn karaoke_band() -> f32 {
    220.0
}

const fn karaoke_width() -> f32 {
    100.0
}

const fn modulation_frequency() -> f32 {
    2.0
}

const fn modulation_depth() -> f32 {
    0.5
}

const fn low_pass_smoothing() -> f32 {
    20.0
}
