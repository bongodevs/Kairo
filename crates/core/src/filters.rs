use thiserror::Error;

/// System equalizer band count, fixed by the wire contract.
pub const BAND_COUNT: usize = 15;
/// Longest echo the fixed delay line can hold.
pub const MAX_ECHO_SECONDS: f32 = 2.0;

const MIN_FACTOR: f64 = 0.25;
const MAX_FACTOR: f64 = 4.0;
const MAX_VIBRATO_HZ: f32 = 14.0;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FilterError {
    #[error("filter volume must be finite and between zero and five")]
    InvalidFilterVolume,
    #[error("echo length must be finite and at least one 48 kHz frame")]
    InvalidEchoLength,
    #[error("echo length exceeds the {max_seconds}-second memory bound")]
    EchoTooLong { max_seconds: f32 },
    #[error("echo decay must be finite and greater than zero")]
    InvalidEchoDecay,
    #[error("tremolo frequency must be finite and greater than zero")]
    InvalidTremoloFrequency,
    #[error("tremolo depth must be finite and between zero and one")]
    InvalidTremoloDepth,
    #[error("vibrato frequency must be finite and in 0 < frequency <= 14")]
    InvalidVibratoFrequency,
    #[error("vibrato depth must be finite and between zero and one")]
    InvalidVibratoDepth,
    #[error("timescale requires a non-zero sample rate and channel count")]
    InvalidTimescaleFormat,
    #[error("timescale input contains {samples} samples, expected interleaved stereo")]
    UnalignedTimescaleInput { samples: usize },
    #[error("timescale processing failed: {message}")]
    Timescale { message: String },
}

/// The complete filter state a client can set on one player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Filters {
    pub volume: f32,
    pub equalizer: EqualizerConfig,
    pub karaoke: Option<KaraokeConfig>,
    pub timescale: Option<TimescaleConfig>,
    pub tremolo: Option<TremoloConfig>,
    pub vibrato: Option<VibratoConfig>,
    pub distortion: Option<DistortionConfig>,
    pub rotation: Option<RotationConfig>,
    pub channel_mix: Option<ChannelMixConfig>,
    pub low_pass: Option<LowPassConfig>,
    pub high_pass: Option<PassConfig>,
    pub cutoff_low_pass: Option<PassConfig>,
    pub normalization: Option<NormalizationConfig>,
    pub echo: Option<EchoConfig>,
}

impl Filters {
    pub fn validate(self) -> Result<(), FilterError> {
        validate_volume(self.volume)?;
        if let Some(config) = self.tremolo.filter(|config| config.is_enabled()) {
            config.validate()?;
        }
        if let Some(config) = self.vibrato.filter(|config| config.is_enabled()) {
            config.validate()?;
        }
        if let Some(config) = self.echo.filter(|config| config.is_enabled()) {
            config.validate()?;
        }
        Ok(())
    }
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            volume: 1.0,
            equalizer: EqualizerConfig::default(),
            karaoke: None,
            timescale: None,
            tremolo: None,
            vibrato: None,
            distortion: None,
            rotation: None,
            channel_mix: None,
            low_pass: None,
            high_pass: None,
            cutoff_low_pass: None,
            normalization: None,
            echo: None,
        }
    }
}

pub fn validate_volume(volume: f32) -> Result<(), FilterError> {
    if !volume.is_finite() || !(0.0..=5.0).contains(&volume) {
        return Err(FilterError::InvalidFilterVolume);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqualizerConfig {
    gains: [f32; BAND_COUNT],
}

impl EqualizerConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self { gains: [0.0; BAND_COUNT] }
    }

    #[must_use]
    pub fn from_bands(bands: impl IntoIterator<Item = (usize, f32)>) -> Self {
        let mut config = Self::new();
        for (band, gain) in bands {
            config.set_gain(band, gain);
        }
        config
    }

    pub fn set_gain(&mut self, band: usize, gain: f32) {
        if let Some(current) = self.gains.get_mut(band) {
            *current = gain.clamp(-0.25, 1.0);
        }
    }

    #[must_use]
    pub const fn gain(&self, band: usize) -> f32 {
        if band < BAND_COUNT { self.gains[band] } else { 0.0 }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.gains.iter().any(|gain| *gain != 0.0)
    }

    #[must_use]
    pub const fn gains(&self) -> &[f32; BAND_COUNT] {
        &self.gains
    }
}

impl Default for EqualizerConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelMixConfig {
    pub left_to_left: f32,
    pub left_to_right: f32,
    pub right_to_left: f32,
    pub right_to_right: f32,
}

impl ChannelMixConfig {
    #[must_use]
    pub fn is_enabled(self) -> bool {
        self != Self::default()
    }
}

impl Default for ChannelMixConfig {
    fn default() -> Self {
        Self { left_to_left: 1.0, left_to_right: 0.0, right_to_left: 0.0, right_to_right: 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LowPassConfig {
    pub smoothing: f32,
}

impl LowPassConfig {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.smoothing > 1.0
    }
}

impl Default for LowPassConfig {
    fn default() -> Self {
        Self { smoothing: 20.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassConfig {
    pub cutoff_hz: u32,
    pub boost: f32,
}

impl PassConfig {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.cutoff_hz > 0
    }
}

impl Default for PassConfig {
    fn default() -> Self {
        Self { cutoff_hz: 0, boost: 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizationConfig {
    pub max_amplitude: f32,
    pub adaptive: bool,
}

impl NormalizationConfig {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.max_amplitude > 0.0
    }
}

impl Default for NormalizationConfig {
    fn default() -> Self {
        Self { max_amplitude: 0.0, adaptive: true }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RotationConfig {
    pub rotation_hz: f64,
}

impl RotationConfig {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.rotation_hz != 0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistortionConfig {
    pub sin_offset: f32,
    pub sin_scale: f32,
    pub cos_offset: f32,
    pub cos_scale: f32,
    pub tan_offset: f32,
    pub tan_scale: f32,
    pub offset: f32,
    pub scale: f32,
}

impl DistortionConfig {
    #[must_use]
    pub fn is_enabled(self) -> bool {
        self != Self::default()
    }
}

impl Default for DistortionConfig {
    fn default() -> Self {
        Self {
            sin_offset: 0.0,
            sin_scale: 1.0,
            cos_offset: 0.0,
            cos_scale: 1.0,
            tan_offset: 0.0,
            tan_scale: 1.0,
            offset: 0.0,
            scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KaraokeConfig {
    pub level: f32,
    pub mono_level: f32,
    pub filter_band: f32,
    pub filter_width: f32,
}

impl Default for KaraokeConfig {
    fn default() -> Self {
        Self { level: 1.0, mono_level: 1.0, filter_band: 220.0, filter_width: 100.0 }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EchoConfig {
    pub length_seconds: f32,
    pub decay: f32,
}

impl EchoConfig {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.length_seconds > 0.0 && self.decay > 0.0
    }

    pub fn validate(self) -> Result<(), FilterError> {
        if !self.length_seconds.is_finite() || self.length_seconds <= 0.0 {
            return Err(FilterError::InvalidEchoLength);
        }
        if self.length_seconds > MAX_ECHO_SECONDS {
            return Err(FilterError::EchoTooLong { max_seconds: MAX_ECHO_SECONDS });
        }
        if !self.decay.is_finite() || self.decay <= 0.0 {
            return Err(FilterError::InvalidEchoDecay);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TremoloConfig {
    pub frequency: f32,
    pub depth: f32,
}

impl TremoloConfig {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.depth != 0.0
    }

    pub fn validate(self) -> Result<(), FilterError> {
        if !self.frequency.is_finite() || self.frequency <= 0.0 {
            return Err(FilterError::InvalidTremoloFrequency);
        }
        if !self.depth.is_finite() || self.depth <= 0.0 || self.depth > 1.0 {
            return Err(FilterError::InvalidTremoloDepth);
        }
        Ok(())
    }
}

impl Default for TremoloConfig {
    fn default() -> Self {
        Self { frequency: 2.0, depth: 0.5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VibratoConfig {
    pub frequency: f32,
    pub depth: f32,
}

impl VibratoConfig {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.depth != 0.0
    }

    pub fn validate(self) -> Result<(), FilterError> {
        if !self.frequency.is_finite() || self.frequency <= 0.0 || self.frequency > MAX_VIBRATO_HZ {
            return Err(FilterError::InvalidVibratoFrequency);
        }
        if !self.depth.is_finite() || self.depth <= 0.0 || self.depth > 1.0 {
            return Err(FilterError::InvalidVibratoDepth);
        }
        Ok(())
    }
}

impl Default for VibratoConfig {
    fn default() -> Self {
        Self { frequency: 2.0, depth: 0.5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimescaleConfig {
    pub speed: f64,
    pub pitch: f64,
    pub rate: f64,
}

impl TimescaleConfig {
    #[must_use]
    pub fn is_enabled(self) -> bool {
        self.speed != 1.0 || self.pitch != 1.0 || self.rate != 1.0
    }

    /// Integer resampling step and WSOLA tempo for this request.
    #[must_use]
    pub fn step_and_tempo(self) -> (f64, f32) {
        let speed = factor(self.speed);
        let pitch = factor(self.pitch);
        let rate = factor(self.rate);
        (
            (pitch * rate).clamp(MIN_FACTOR, MAX_FACTOR),
            (speed / pitch).clamp(MIN_FACTOR, MAX_FACTOR) as f32,
        )
    }

    /// How fast the source timeline advances per output second.
    #[must_use]
    pub fn source_speed(self) -> f64 {
        let (step, tempo) = self.step_and_tempo();
        step * f64::from(tempo)
    }
}

impl Default for TimescaleConfig {
    fn default() -> Self {
        Self { speed: 1.0, pitch: 1.0, rate: 1.0 }
    }
}

fn factor(value: f64) -> f64 {
    if value.is_finite() { value.clamp(MIN_FACTOR, MAX_FACTOR) } else { 1.0 }
}
