//! Wire forms of the per-player transition overrides.

use serde::{Deserialize, Serialize};

use player::effects::{CrossfadeOptions, TapeCurve as EngineTapeCurve, TapeOptions};

use crate::config::CrossfadeCurve;

fn default_crossfade_duration_ms() -> u64 {
    6000
}

fn default_manual_crossfade_duration_ms() -> u64 {
    3500
}

fn default_tape_duration_ms() -> u64 {
    500
}

/// Longest accepted tape ramp. An unbounded duration would drive the per-frame step to zero, so a
/// paused player would never reach silence.
const MAX_TAPE_DURATION_MS: u64 = 60_000;

/// Per-player transition override, the `crossfade` field of a player update.
///
/// A present value replaces the node-wide defaults for that player, and `null` clears the
/// override. `enable` with `durationMs` drives a fade overlap, `gapless` alone a zero overlap, and
/// with both off a track simply finishes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CrossfadeSettings {
    pub(crate) enable: bool,
    #[serde(default = "default_crossfade_duration_ms")]
    pub(crate) duration_ms: u64,
    #[serde(default = "default_manual_crossfade_duration_ms")]
    pub(crate) manual_duration_ms: u64,
    pub(crate) curve: CrossfadeCurve,
    pub(crate) gapless: bool,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self {
            enable: false,
            duration_ms: default_crossfade_duration_ms(),
            manual_duration_ms: default_manual_crossfade_duration_ms(),
            curve: CrossfadeCurve::SCurve,
            gapless: false,
        }
    }
}

impl CrossfadeSettings {
    /// Convert to engine options, or `None` when no transition would happen.
    #[must_use]
    pub(crate) fn to_engine(self) -> Option<CrossfadeOptions> {
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

    /// The wire view of the options in effect.
    #[must_use]
    pub(crate) fn from_engine(options: &CrossfadeOptions) -> Self {
        Self {
            enable: options.duration_ms > 0,
            duration_ms: options.duration_ms,
            manual_duration_ms: options.manual_duration_ms,
            curve: CrossfadeCurve::from_engine(options.curve),
            gapless: options.gapless,
        }
    }
}

/// Per-player pause and resume ramp, the `tape` field of a player update.
///
/// There is no node-wide default, so `null` and `enable: false` both switch it off.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct TapeSettings {
    pub(crate) enable: bool,
    #[serde(default = "default_tape_duration_ms")]
    pub(crate) duration_ms: u64,
    pub(crate) curve: TapeCurve,
}

impl Default for TapeSettings {
    fn default() -> Self {
        Self { enable: false, duration_ms: default_tape_duration_ms(), curve: TapeCurve::SCurve }
    }
}

impl TapeSettings {
    /// Convert to engine options, or `None` when disabled.
    #[must_use]
    pub(crate) fn to_engine(self) -> Option<TapeOptions> {
        if !self.enable {
            return None;
        }
        Some(TapeOptions {
            duration_ms: self.duration_ms.min(MAX_TAPE_DURATION_MS),
            curve: match self.curve {
                TapeCurve::Linear => EngineTapeCurve::Linear,
                TapeCurve::Exponential => EngineTapeCurve::Exponential,
                TapeCurve::SCurve => EngineTapeCurve::SCurve,
                TapeCurve::Quad => EngineTapeCurve::Quad,
            },
        })
    }
}

/// Ramp easing for [`TapeSettings`].
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TapeCurve {
    /// Constant-rate ramp.
    Linear,
    /// Steep near full speed, slow near the stop.
    Exponential,
    /// Smooth S-curve, the default.
    #[default]
    SCurve,
    /// Quadratic ease.
    Quad,
}
