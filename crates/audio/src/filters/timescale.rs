#[cfg(test)]
#[path = "../../tests/filters_scale.rs"]
mod scale_tests;

use std::num::{NonZeroU16, NonZeroU32};

use kairo_core::filters::{FilterError, TimescaleConfig};

use crate::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE};

const MAX_RESAMPLED_FRAMES: usize = FRAME_SAMPLES * 4;
const HOP_MS: f32 = 15.0;
const SEARCH_MS: f32 = 3.0;
const WARMUP_FRAMES: usize = (1 << 16) + 8_192;
static WARMUP_SILENCE: [f32; FRAME_SAMPLES * CHANNELS] = [0.0; FRAME_SAMPLES * CHANNELS];

pub struct Timescale {
    stretch: wsola::TimeStretch,
    step: f64,
    phase: f64,
    previous: [f32; CHANNELS],
    has_previous: bool,
    resampled: Box<[f32]>,
    output: Box<[f32]>,
}

impl Timescale {
    pub fn new(config: TimescaleConfig) -> Result<Self, FilterError> {
        let sample_rate =
            NonZeroU32::new(SAMPLE_RATE).ok_or(FilterError::InvalidTimescaleFormat)?;
        let channels =
            NonZeroU16::new(CHANNELS as u16).ok_or(FilterError::InvalidTimescaleFormat)?;
        let mut stretch = wsola::TimeStretch::with_config(
            sample_rate,
            channels,
            wsola::Config { hop_ms: HOP_MS, search_ms: SEARCH_MS },
        )
        .map_err(|error| FilterError::Timescale { message: error.to_string() })?;
        warm_up(&mut stretch);
        stretch.reset();
        let output = vec![0.0; stretch.min_output_buff_size() * 4].into_boxed_slice();
        let (step, tempo) = config.step_and_tempo();
        stretch.set_tempo(tempo);
        Ok(Self {
            stretch,
            step,
            phase: 0.0,
            previous: [0.0; CHANNELS],
            has_previous: false,
            resampled: vec![0.0; MAX_RESAMPLED_FRAMES * CHANNELS].into_boxed_slice(),
            output,
        })
    }

    pub fn process<F>(&mut self, input: &[f32], mut emit: F) -> Result<(), FilterError>
    where
        F: FnMut(&[f32]),
    {
        if !input.len().is_multiple_of(CHANNELS) {
            return Err(FilterError::UnalignedTimescaleInput { samples: input.len() });
        }
        let written = self.resample(input);
        self.stretch.push(&self.resampled[..written]);
        self.pull(&mut emit)
    }

    pub fn drain<F>(&mut self, mut emit: F) -> Result<(), FilterError>
    where
        F: FnMut(&[f32]),
    {
        if self.has_previous {
            let tail = self.previous;
            let written = self.resample(&tail);
            self.stretch.push(&self.resampled[..written]);
        }
        self.stretch.set_draining();
        let result = self.pull(&mut emit);
        if result.is_ok() {
            self.phase = 0.0;
            self.has_previous = false;
        }
        result
    }

    pub fn reset(&mut self) {
        self.stretch.reset();
        self.phase = 0.0;
        self.has_previous = false;
    }

    fn pull<F>(&mut self, emit: &mut F) -> Result<(), FilterError>
    where
        F: FnMut(&[f32]),
    {
        loop {
            match self.stretch.pull(&mut self.output) {
                Ok(written) => emit(&self.output[..written]),
                Err(wsola::Error::NotEnoughInput) => return Ok(()),
                Err(error) => return Err(FilterError::Timescale { message: error.to_string() }),
            }
        }
    }

    fn resample(&mut self, input: &[f32]) -> usize {
        let frames = input.len() / CHANNELS;
        if frames == 0 {
            return 0;
        }
        if !self.has_previous {
            self.previous.copy_from_slice(&input[..CHANNELS]);
            self.phase = 1.0;
            self.has_previous = true;
        }
        let total = frames + 1;
        let mut written_frames = 0;
        while self.phase + 1.0 < total as f64 && written_frames < MAX_RESAMPLED_FRAMES {
            let index = self.phase.floor() as usize;
            let fraction = (self.phase - index as f64) as f32;
            for channel in 0..CHANNELS {
                let first = self.sample(input, index, channel);
                let second = self.sample(input, index + 1, channel);
                self.resampled[written_frames * CHANNELS + channel] =
                    first + (second - first) * fraction;
            }
            written_frames += 1;
            self.phase += self.step;
        }
        self.phase -= (total - 1) as f64;
        let last = (frames - 1) * CHANNELS;
        self.previous.copy_from_slice(&input[last..last + CHANNELS]);
        written_frames * CHANNELS
    }

    fn sample(&self, input: &[f32], index: usize, channel: usize) -> f32 {
        if index == 0 { self.previous[channel] } else { input[(index - 1) * CHANNELS + channel] }
    }
}

fn warm_up(stretch: &mut wsola::TimeStretch) {
    let mut frames = WARMUP_FRAMES;
    while frames > 0 {
        let chunk = frames.min(FRAME_SAMPLES);
        stretch.push(&WARMUP_SILENCE[..chunk * CHANNELS]);
        frames -= chunk;
    }
}
