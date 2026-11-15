//! `crossfade.*`, the node-wide track transition defaults.
//!
//! A client can override these per player through the `crossfade` field of a player update. With
//! both a fade and a gapless hand-off off, a track simply ends with reason `finished`.

use serde::{Deserialize, Serialize};

use player::effects::{CrossfadeCurve as EngineCrossfadeCurve, CrossfadeOptions};

/// The `crossfade` block.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CrossfadeConfig {
    /// Whether to overlap the outgoing and incoming tracks on a transition.
    pub enable: bool,
    /// Overlap length in milliseconds, used when `enable` is set.
    pub duration_ms: u64,
    /// Overlap length in milliseconds for an explicit manual skip.
    pub manual_duration_ms: u64,
    /// Fade easing.
    pub curve: CrossfadeCurve,
    /// Whether to fall back to a zero-gap hand-off when a fade is off or cannot apply.
    pub gapless: bool,
}

impl Default for CrossfadeConfig {
    fn default() -> Self {
        Self {
            enable: false,
            duration_ms: 6000,
            manual_duration_ms: 3500,
            curve: CrossfadeCurve::SCurve,
            gapless: true,
        }
    }
}

impl CrossfadeConfig {
    /// The transition options in effect, or `None` when neither a fade nor a gapless hand-off is
    /// wanted. A gapless hand-off is the same transition with a zero overlap.
    #[must_use]
    pub fn to_engine(&self) -> Option<CrossfadeOptions> {
        let duration_ms = if self.enable { self.duration_ms } else { 0 };
        if duration_ms == 0 && !self.gapless {
            return None;
        }
        Some(CrossfadeOptions {
            duration_ms,
            manual_duration_ms: self.manual_duration_ms,
            curve: self.curve.to_engine(),
            gapless: self.gapless,
        })
    }
}

/// Fade easing as written in TOML and on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CrossfadeCurve {
    /// Straight ramp.
    Linear,
    /// Fast start, slow end.
    Exp,
    /// Slow start, fast end.
    Log,
    /// Raised-cosine S-curve, the default.
    #[default]
    SCurve,
    /// Constant-power sine/cosine pair.
    Sinusoidal,
}

impl CrossfadeCurve {
    pub(crate) const fn to_engine(self) -> EngineCrossfadeCurve {
        match self {
            Self::Linear => EngineCrossfadeCurve::Linear,
            Self::Exp => EngineCrossfadeCurve::Exponential,
            Self::Log => EngineCrossfadeCurve::Logarithmic,
            Self::SCurve => EngineCrossfadeCurve::SCurve,
            Self::Sinusoidal => EngineCrossfadeCurve::Sinusoidal,
        }
    }

    pub(crate) const fn from_engine(curve: EngineCrossfadeCurve) -> Self {
        match curve {
            EngineCrossfadeCurve::Linear => Self::Linear,
            EngineCrossfadeCurve::Exponential => Self::Exp,
            EngineCrossfadeCurve::Logarithmic => Self::Log,
            EngineCrossfadeCurve::SCurve => Self::SCurve,
            EngineCrossfadeCurve::Sinusoidal => Self::Sinusoidal,
        }
    }
}
