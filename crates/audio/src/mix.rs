#[cfg(test)]
#[path = "../tests/effects_crossfade.rs"]
mod crossfade_tests;
#[cfg(test)]
#[path = "../tests/effects_tape.rs"]
mod tape_tests;
#[cfg(test)]
#[path = "../tests/volume.rs"]
mod volume_tests;

use opus_rs::{Application, OpusDecoder, OpusEncoder};

use kairo_core::Millis;

use crate::opus::OpusEncoderConfig;
use crate::{CHANNELS, FRAME_SAMPLES, MAX_PACKET_BYTES, SAMPLE_RATE};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossfadeCurve {
    Linear,
    Exponential,
    Logarithmic,
    SCurve,
    Sinusoidal,
}

impl CrossfadeCurve {
    #[must_use]
    pub fn ease(self, progress: f64) -> f64 {
        let progress = progress.clamp(0.0, 1.0);
        match self {
            Self::Linear => progress,
            Self::Exponential => progress * progress,
            Self::Logarithmic => progress.sqrt(),
            Self::SCurve => (1.0 - (std::f64::consts::PI * progress).cos()) / 2.0,
            Self::Sinusoidal => (progress * std::f64::consts::FRAC_PI_2).sin(),
        }
    }

    #[must_use]
    pub fn ease_out(self, progress: f64) -> f64 {
        match self {
            Self::Sinusoidal => (progress.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).cos(),
            _ => 1.0 - self.ease(progress),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossfadeOptions {
    pub duration_ms: u64,
    pub manual_duration_ms: u64,
    pub curve: CrossfadeCurve,
    pub gapless: bool,
}

impl CrossfadeOptions {
    #[must_use]
    pub fn is_gapless(self) -> bool {
        self.duration_ms == 0 && self.gapless
    }
}

const FRAME_MS: u64 = 20;
const MAX_DURATION_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapeCurve {
    Linear,
    Exponential,
    SCurve,
    Quad,
}

impl TapeCurve {
    pub(crate) fn ease(self, progress: f64) -> f64 {
        let progress = progress.clamp(0.0, 1.0);
        match self {
            Self::Linear => progress,
            Self::Exponential => {
                if progress <= 0.0 {
                    0.0
                } else {
                    2f64.powf(10.0 * (progress - 1.0))
                }
            }
            Self::SCurve => (1.0 - (std::f64::consts::PI * progress).cos()) / 2.0,
            Self::Quad => progress * progress,
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            Self::Linear => 0,
            Self::Exponential => 1,
            Self::SCurve => 2,
            Self::Quad => 3,
        }
    }

    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Linear,
            1 => Self::Exponential,
            3 => Self::Quad,
            _ => Self::SCurve,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapeOptions {
    pub duration_ms: u64,
    pub curve: TapeCurve,
}

pub struct TapeEffect {
    options: TapeOptions,
    phase_pos: f64,
    target: f64,
    step: f64,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    cursor: f64,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    last_timecode: u64,
}

impl TapeEffect {
    #[must_use]
    pub fn new(options: TapeOptions) -> Self {
        let options = clamp_options(options);
        Self {
            options,
            phase_pos: 1.0,
            target: 1.0,
            step: Self::step_for(options.duration_ms),
            in_l: Vec::new(),
            in_r: Vec::new(),
            cursor: 0.0,
            out_l: Vec::new(),
            out_r: Vec::new(),
            last_timecode: 0,
        }
    }

    pub fn reset(&mut self) {
        self.phase_pos = if self.target >= 1.0 { 1.0 } else { 0.0 };
        self.in_l.clear();
        self.in_r.clear();
        self.cursor = 0.0;
        self.out_l.clear();
        self.out_r.clear();
    }

    pub fn set_options(&mut self, options: TapeOptions) {
        let options = clamp_options(options);
        self.step = Self::step_for(options.duration_ms);
        self.options = options;
    }

    pub fn snap_paused(&mut self) {
        self.target = 0.0;
        self.reset();
    }

    pub fn begin_pause(&mut self) {
        self.target = 0.0;
    }

    pub fn begin_resume(&mut self) {
        self.target = 1.0;
    }

    #[must_use]
    pub fn is_inactive(&self) -> bool {
        self.target >= 1.0 && self.phase_pos >= 1.0 && self.out_l.is_empty()
    }

    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.target <= 0.0 && self.phase_pos <= 0.0
    }

    #[must_use]
    pub fn is_transitioning(&self) -> bool {
        !self.is_inactive() && !self.is_silent()
    }

    pub fn push_input(&mut self, pcm: &[f32], timecode: u64) {
        let frames = pcm.len() / CHANNELS;
        self.in_l.reserve(frames);
        self.in_r.reserve(frames);
        for frame in pcm.as_chunks::<CHANNELS>().0 {
            self.in_l.push(frame[0]);
            self.in_r.push(frame[1]);
        }
        self.last_timecode = timecode;
    }

    pub fn try_emit_into(&mut self, out: &mut Vec<f32>) -> Option<u64> {
        if self.is_silent() {
            return None;
        }
        let rate = self.rate();
        while self.out_l.len() < FRAME_SAMPLES {
            let index = self.cursor.floor() as usize;
            let fraction = self.cursor - index as f64;
            let needed = if fraction > 0.0 { index + 1 } else { index };
            if needed >= self.in_l.len() {
                return None;
            }
            let (left, right) = if fraction > 0.0 {
                let fraction = fraction as f32;
                (
                    self.in_l[index] + (self.in_l[index + 1] - self.in_l[index]) * fraction,
                    self.in_r[index] + (self.in_r[index + 1] - self.in_r[index]) * fraction,
                )
            } else {
                (self.in_l[index], self.in_r[index])
            };
            self.out_l.push(left);
            self.out_r.push(right);
            self.cursor += rate;
        }

        let consumed = self.cursor.floor() as usize;
        if consumed > 0 {
            let consumed = consumed.min(self.in_l.len());
            self.in_l.drain(0..consumed);
            self.in_r.drain(0..consumed);
            self.cursor -= consumed as f64;
        }

        out.clear();
        out.reserve(FRAME_SAMPLES * CHANNELS);
        for (left, right) in self.out_l.iter().zip(&self.out_r) {
            out.push(*left);
            out.push(*right);
        }
        self.out_l.clear();
        self.out_r.clear();
        let timecode = self.last_timecode;

        let remaining = self.target - self.phase_pos;
        if remaining.abs() <= self.step + 1e-9 {
            self.phase_pos = self.target;
        } else {
            self.phase_pos += self.step.copysign(remaining);
        }
        Some(timecode)
    }

    fn rate(&self) -> f64 {
        self.options.curve.ease(self.phase_pos)
    }

    fn step_for(duration_ms: u64) -> f64 {
        let frames = duration_ms.div_ceil(FRAME_MS).max(1);
        1.0 / frames as f64
    }
}

fn clamp_options(options: TapeOptions) -> TapeOptions {
    TapeOptions { duration_ms: options.duration_ms.min(MAX_DURATION_MS), curve: options.curve }
}

pub const DEFAULT_VOLUME: u16 = 100;
pub const MAX_VOLUME: u16 = 1_000;

/// A clamped player volume percentage in the protocol's 0..=1000 range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume(u16);

impl Volume {
    pub const SILENT: Self = Self(0);
    pub const UNITY: Self = Self(DEFAULT_VOLUME);
    pub const MAX: Self = Self(MAX_VOLUME);

    #[must_use]
    pub fn new(level: i32) -> Self {
        Self(level.clamp(0, i32::from(MAX_VOLUME)) as u16)
    }

    #[must_use]
    pub const fn level(self) -> u16 {
        self.0
    }

    /// Apply the player loudness curve in place, including hard clipping.
    pub fn apply(self, samples: &mut [f32]) {
        if self == Self::UNITY {
            return;
        }
        apply_gain(samples, self.gain());
    }

    pub fn gain(self) -> f32 {
        if self == Self::UNITY { 1.0 } else { self.multiplier() as f32 / 10_000.0 }
    }

    fn multiplier(self) -> i32 {
        if self.0 <= 150 {
            ((f32::from(self.0) * 0.0079).tan() * 10_000.0) as i32
        } else {
            24_621 * i32::from(self.0) / 150
        }
    }
}

pub fn apply_gain(samples: &mut [f32], gain: f32) {
    const POSITIVE_LIMIT: f32 = i16::MAX as f32 / 32_768.0;
    for sample in samples {
        *sample = (*sample * gain).clamp(-1.0, POSITIVE_LIMIT);
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::UNITY
    }
}

impl From<i32> for Volume {
    fn from(level: i32) -> Self {
        Self::new(level)
    }
}

const RAMP_FRAMES: i32 = 50;

pub(crate) struct VolumeRamp {
    start: Volume,
    current: Volume,
    target: Volume,
    frame: i32,
    gain: f32,
}

impl VolumeRamp {
    pub(crate) fn new(volume: Volume) -> Self {
        Self {
            start: volume,
            current: volume,
            target: volume,
            frame: RAMP_FRAMES,
            gain: volume.gain(),
        }
    }

    pub(crate) fn set(&mut self, target: Volume) {
        if target == self.target {
            return;
        }
        self.start = self.current;
        self.target = target;
        self.frame = 0;
    }

    pub(crate) fn set_immediate(&mut self, volume: Volume) {
        *self = Self::new(volume);
    }

    pub(crate) fn route_level(&self) -> u16 {
        if self.current == Volume::UNITY && self.target == Volume::UNITY {
            Volume::UNITY.level()
        } else if self.target != Volume::UNITY {
            self.target.level()
        } else {
            self.current.level()
        }
    }

    pub(crate) fn apply_frame(&mut self, samples: &mut [f32]) -> Volume {
        let volume = self.next_volume();
        if volume != Volume::UNITY {
            apply_gain(samples, self.gain);
        }
        volume
    }

    fn next_volume(&mut self) -> Volume {
        if self.current == self.target {
            return self.current;
        }
        self.frame += 1;
        self.current = if self.frame >= RAMP_FRAMES {
            self.target
        } else {
            let start = i32::from(self.start.level());
            let target = i32::from(self.target.level());
            Volume::new(start + (target - start) * self.frame / RAMP_FRAMES)
        };
        self.gain = self.current.gain();
        self.current
    }
}

/// Consumer-side tape-machine ramp for one player.
pub struct TapeRuntime {
    effect: TapeEffect,
    decoder: OpusDecoder,
    encoder: OpusEncoder,
    decoded: Vec<f32>,
    ramp: Vec<f32>,
    packet: [u8; MAX_PACKET_BYTES],
    clock: Millis,
    volume: Volume,
}

impl TapeRuntime {
    pub fn new(options: TapeOptions, position: Millis) -> Result<Self, &'static str> {
        let mut encoder = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio)?;
        OpusEncoderConfig::default().apply_to(&mut encoder);
        Ok(Self {
            effect: TapeEffect::new(options),
            decoder: OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS)?,
            encoder,
            decoded: vec![0.0; FRAME_SAMPLES * CHANNELS],
            ramp: Vec::with_capacity(FRAME_SAMPLES * CHANNELS),
            packet: [0; MAX_PACKET_BYTES],
            clock: position,
            volume: Volume::UNITY,
        })
    }

    pub fn set_options(&mut self, options: TapeOptions) {
        self.effect.set_options(options);
    }

    pub fn reset(&mut self, position: Millis) {
        self.effect.reset();
        self.clock = position;
    }

    pub fn snap_paused(&mut self, position: Millis) {
        self.effect.snap_paused();
        self.clock = position;
    }

    pub fn begin_pause(&mut self) {
        self.effect.begin_pause();
    }

    pub fn begin_resume(&mut self) {
        self.effect.begin_resume();
    }

    pub fn is_silent(&self) -> bool {
        self.effect.is_silent()
    }

    pub fn is_inactive(&self) -> bool {
        self.effect.is_inactive()
    }

    /// Decode one buffered packet and queue its samples for the ramp.
    pub fn push_packet(&mut self, packet: &[u8], volume: Volume) -> Result<(), &'static str> {
        let frames = self.decoder.decode(packet, FRAME_SAMPLES, &mut self.decoded)?;
        if frames == 0 {
            return Ok(());
        }
        self.effect.push_input(&self.decoded[..frames * CHANNELS], self.clock.get());
        self.volume = volume;
        Ok(())
    }

    /// Encode one ramped 20 ms frame. `None` means the ramp is stopped or needs more input.
    pub fn try_emit(
        &mut self,
        output: &mut [u8],
    ) -> Result<Option<(usize, Millis, Volume)>, &'static str> {
        if self.effect.is_silent() {
            return Ok(None);
        }
        if self.effect.try_emit_into(&mut self.ramp).is_none() {
            return Ok(None);
        }
        let length = self.encoder.encode(&self.ramp, FRAME_SAMPLES, &mut self.packet)?;
        if length > output.len() {
            return Err("output buffer too small for a ramped frame");
        }
        output[..length].copy_from_slice(&self.packet[..length]);
        let position = self.clock;
        self.clock = Millis::new(position.get().saturating_add(20));
        Ok(Some((length, position, self.volume)))
    }
}
