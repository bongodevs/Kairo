use crate::effects::{CrossfadeCurve, CrossfadeOptions};
use crate::opus::OpusEncoderConfig;
use crate::pcm::ResamplingQuality;
use crate::pipeline::FrameBufferConfig;
pub const DEFAULT_STUCK_THRESHOLD_MS: u64 = 10_000;
pub const MIN_STUCK_THRESHOLD_MS: u64 = 100;
pub const DEFAULT_CLEANUP_THRESHOLD_MS: u64 = 60_000;
pub const DEFAULT_MANUAL_CROSSFADE_DURATION_MS: u64 = 3_500;

pub const fn default_crossfade() -> Option<CrossfadeOptions> {
    Some(CrossfadeOptions {
        duration_ms: 0,
        manual_duration_ms: DEFAULT_MANUAL_CROSSFADE_DURATION_MS,
        curve: CrossfadeCurve::SCurve,
        gapless: true,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerConfig {
    pub frame_buffer: FrameBufferConfig,
    pub resampling_quality: ResamplingQuality,
    pub opus: OpusEncoderConfig,
    pub track_stuck_threshold_ms: u64,
    pub cleanup_threshold_ms: u64,
    pub use_seek_ghosting: bool,
    /// Transition defaults when the client has no per-player override.
    pub crossfade: Option<CrossfadeOptions>,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            frame_buffer: FrameBufferConfig::default(),
            resampling_quality: ResamplingQuality::default(),
            opus: OpusEncoderConfig::default(),
            track_stuck_threshold_ms: DEFAULT_STUCK_THRESHOLD_MS,
            cleanup_threshold_ms: DEFAULT_CLEANUP_THRESHOLD_MS,
            use_seek_ghosting: true,
            crossfade: default_crossfade(),
        }
    }
}
