use serde::Deserialize;

use player::filters::{EchoConfig, NormalizationConfig, PassConfig};

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Pass {
    #[serde(default)]
    cutoff_frequency: u32,
    #[serde(default = "one_f32")]
    boost_factor: f32,
}

impl From<Pass> for PassConfig {
    fn from(value: Pass) -> Self {
        Self { cutoff_hz: value.cutoff_frequency, boost: value.boost_factor }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Normalization {
    #[serde(default)]
    max_amplitude: f32,
    #[serde(default = "true_value")]
    adaptive: bool,
}

impl From<Normalization> for NormalizationConfig {
    fn from(value: Normalization) -> Self {
        Self { max_amplitude: value.max_amplitude, adaptive: value.adaptive }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Echo {
    #[serde(default)]
    echo_length: f32,
    #[serde(default)]
    decay: f32,
}

impl From<Echo> for EchoConfig {
    fn from(value: Echo) -> Self {
        Self { length_seconds: value.echo_length, decay: value.decay }
    }
}

const fn one_f32() -> f32 {
    1.0
}

const fn true_value() -> bool {
    true
}
