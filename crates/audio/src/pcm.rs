#[cfg(test)]
#[path = "../tests/pcm_normalize.rs"]
mod normalize_tests;
#[cfg(test)]
#[path = "../tests/pcm_pipeline.rs"]
mod pipeline_tests;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Indexing, PolynomialDegree, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};
use thiserror::Error;

use kairo_core::{AudioFormat, Millis};

use crate::clock::add_frames;
use crate::decode::PcmBlock;
use crate::{
    CHANNELS, MAX_DRAIN_CHUNKS, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE, RESAMPLE_CHUNK_FRAMES,
    SAMPLE_RATE,
};

/// Failure while normalizing native PCM to 48 kHz stereo.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PcmError {
    #[error("PCM block contains {actual} samples, expected {expected}")]
    InvalidBlock { expected: usize, actual: usize },

    #[error("sample rate {sample_rate} Hz is outside the supported range")]
    UnsupportedSampleRate { sample_rate: u32 },

    #[error("PCM block must contain at least one channel")]
    InvalidChannels,

    #[error("failed to construct the sample-rate converter")]
    Construction {
        #[source]
        source: rubato::ResamplerConstructionError,
    },

    #[error("sample-rate conversion failed")]
    Resample {
        #[source]
        source: rubato::ResampleError,
    },

    #[error("the fixed PCM buffer has an invalid shape")]
    Buffer {
        #[source]
        source: rubato::audioadapter_buffers::SizeError,
    },

    #[error("the sample-rate converter could not drain its buffered tail")]
    IncompleteDrain,

    #[error("PCM was submitted after the stream had finished")]
    Finished,
}

/// Cost/quality choice matching the three player configuration levels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResamplingQuality {
    #[default]
    Low,
    Medium,
    High,
}

/// Sinc window width for `MEDIUM` and `HIGH`, in taps.
const MEDIUM_TAPS: usize = 64;
const HIGH_TAPS: usize = 128;

/// Table steps per input sample, setting the sub-sample resolution of the sinc kernel.
const SINC_OVERSAMPLING: usize = 128;

fn sinc_parameters(taps: usize) -> SincInterpolationParameters {
    SincInterpolationParameters::new(taps, WindowFunction::BlackmanHarris2)
        .oversampling_factor(SINC_OVERSAMPLING)
        .interpolation(SincInterpolationType::Linear)
}

pub(super) fn make_resampler(
    quality: ResamplingQuality,
    source_rate: u32,
    target_rate: u32,
    chunk_frames: usize,
    channels: usize,
) -> Result<Async<f32>, PcmError> {
    let ratio = f64::from(target_rate) / f64::from(source_rate);
    let result = match quality {
        ResamplingQuality::Low => Async::new_poly(
            ratio,
            1.0,
            PolynomialDegree::Linear,
            chunk_frames,
            channels,
            FixedAsync::Input,
        ),
        ResamplingQuality::Medium => Async::new_sinc(
            ratio,
            1.0,
            &sinc_parameters(MEDIUM_TAPS),
            chunk_frames,
            channels,
            FixedAsync::Input,
        ),
        ResamplingQuality::High => Async::new_sinc(
            ratio,
            1.0,
            &sinc_parameters(HIGH_TAPS),
            chunk_frames,
            channels,
            FixedAsync::Input,
        ),
    };
    result.map_err(|source| PcmError::Construction { source })
}

pub(super) fn validate_block(block: &PcmBlock<'_>) -> Result<(), PcmError> {
    if block.format.channels == 0 {
        return Err(PcmError::InvalidChannels);
    }
    let expected = block.frames.saturating_mul(block.format.channels);
    if block.samples.len() != expected {
        return Err(PcmError::InvalidBlock { expected, actual: block.samples.len() });
    }
    Ok(())
}

pub(super) fn copy_stereo(
    input: &[f32],
    channels: usize,
    input_offset: usize,
    frames: usize,
    output: &mut [f32],
) {
    if channels == CHANNELS {
        let start = input_offset * CHANNELS;
        output.copy_from_slice(&input[start..start + frames * CHANNELS]);
        return;
    }

    for frame in 0..frames {
        let source = (input_offset + frame) * channels;
        let target = frame * CHANNELS;
        output[target] = input[source];
        output[target + 1] = if channels == 1 { input[source] } else { input[source + 1] };
    }
}

pub const OUTPUT_FORMAT: AudioFormat = AudioFormat { sample_rate: SAMPLE_RATE, channels: CHANNELS };

/// Worker-owned channel normalizer and streaming sample-rate converter.
pub struct PcmNormalizer {
    quality: ResamplingQuality,
    format: Option<AudioFormat>,
    resampler: Option<Async<f32>>,
    input: Box<[f32]>,
    input_fill: usize,
    output: Box<[f32]>,
    delay_to_trim: usize,
    real_input_frames: u64,
    emitted_frames: u64,
    base_position: Millis,
    finished: bool,
}

impl PcmNormalizer {
    #[must_use]
    pub fn new(quality: ResamplingQuality) -> Self {
        Self {
            quality,
            format: None,
            resampler: None,
            input: vec![0.0; RESAMPLE_CHUNK_FRAMES * CHANNELS].into_boxed_slice(),
            input_fill: 0,
            output: Box::default(),
            delay_to_trim: 0,
            real_input_frames: 0,
            emitted_frames: 0,
            base_position: Millis::default(),
            finished: false,
        }
    }

    #[must_use]
    pub fn input_format(&self) -> Option<AudioFormat> {
        self.format
    }

    /// Normalize one decoder block and emit zero or more borrowed 48 kHz stereo blocks.
    pub fn process<F>(&mut self, block: PcmBlock<'_>, mut emit: F) -> Result<(), PcmError>
    where
        F: for<'a> FnMut(PcmBlock<'a>),
    {
        if self.finished {
            return Err(PcmError::Finished);
        }
        validate_block(&block)?;
        if block.frames == 0 {
            return Ok(());
        }
        if self.format != Some(block.format) {
            self.finish_inner(&mut emit)?;
            self.configure(block.format, block.position)?;
        }

        if self.resampler.is_none() {
            return self.process_native(block, &mut emit);
        }
        self.process_resampled(block, &mut emit)
    }

    /// Emit the buffered tail and make further input fail until [`Self::reset`].
    pub fn finish<F>(&mut self, mut emit: F) -> Result<(), PcmError>
    where
        F: for<'a> FnMut(PcmBlock<'a>),
    {
        self.finish_inner(&mut emit)
    }

    /// Clear buffered audio after a seek or discontinuity while retaining the current format.
    pub fn reset(&mut self, position: Millis) {
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
            self.delay_to_trim = resampler.output_delay();
        }
        self.input.fill(0.0);
        self.input_fill = 0;
        self.real_input_frames = 0;
        self.emitted_frames = 0;
        self.base_position = position;
        self.finished = false;
    }

    fn configure(&mut self, format: AudioFormat, position: Millis) -> Result<(), PcmError> {
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&format.sample_rate) {
            return Err(PcmError::UnsupportedSampleRate { sample_rate: format.sample_rate });
        }
        self.format = Some(format);
        self.resampler = if format.sample_rate == SAMPLE_RATE {
            None
        } else {
            Some(make_resampler(
                self.quality,
                format.sample_rate,
                SAMPLE_RATE,
                RESAMPLE_CHUNK_FRAMES,
                CHANNELS,
            )?)
        };
        if let Some(resampler) = self.resampler.as_ref() {
            let samples = resampler
                .output_frames_max()
                .checked_mul(CHANNELS)
                .ok_or(PcmError::UnsupportedSampleRate { sample_rate: format.sample_rate })?;
            self.output = vec![0.0; samples].into_boxed_slice();
        } else {
            self.output = Box::default();
        }
        self.reset(position);
        Ok(())
    }

    fn process_native<F>(&mut self, block: PcmBlock<'_>, emit: &mut F) -> Result<(), PcmError>
    where
        F: for<'a> FnMut(PcmBlock<'a>),
    {
        if block.format == OUTPUT_FORMAT {
            emit(block);
            return Ok(());
        }

        let mut offset = 0;
        while offset < block.frames {
            let frames = (block.frames - offset).min(RESAMPLE_CHUNK_FRAMES);
            copy_stereo(
                block.samples,
                block.format.channels,
                offset,
                frames,
                &mut self.input[..frames * CHANNELS],
            );
            emit(PcmBlock {
                samples: &self.input[..frames * CHANNELS],
                format: OUTPUT_FORMAT,
                frames,
                position: add_frames(block.position, offset as u128, SAMPLE_RATE),
            });
            offset += frames;
        }
        Ok(())
    }

    fn process_resampled<F>(&mut self, block: PcmBlock<'_>, emit: &mut F) -> Result<(), PcmError>
    where
        F: for<'a> FnMut(PcmBlock<'a>),
    {
        let mut offset = 0;
        while offset < block.frames {
            let frames = (block.frames - offset).min(RESAMPLE_CHUNK_FRAMES - self.input_fill);
            let output_start = self.input_fill * CHANNELS;
            copy_stereo(
                block.samples,
                block.format.channels,
                offset,
                frames,
                &mut self.input[output_start..output_start + frames * CHANNELS],
            );
            self.input_fill += frames;
            self.real_input_frames =
                self.real_input_frames.saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
            offset += frames;
            if self.input_fill == RESAMPLE_CHUNK_FRAMES {
                self.run_resampler(None, emit)?;
                self.input_fill = 0;
            }
        }
        Ok(())
    }

    fn finish_inner<F>(&mut self, emit: &mut F) -> Result<(), PcmError>
    where
        F: for<'a> FnMut(PcmBlock<'a>),
    {
        if self.finished || self.format.is_none() {
            return Ok(());
        }
        if self.resampler.is_none() {
            self.finished = true;
            return Ok(());
        }

        if self.input_fill > 0 {
            self.input[self.input_fill * CHANNELS..].fill(0.0);
            self.run_resampler(Some(self.input_fill), emit)?;
            self.input_fill = 0;
        }
        for _ in 0..MAX_DRAIN_CHUNKS {
            if self.emitted_frames >= self.target_frames() {
                self.finished = true;
                return Ok(());
            }
            self.input.fill(0.0);
            self.run_resampler(Some(0), emit)?;
        }
        Err(PcmError::IncompleteDrain)
    }

    fn run_resampler<F>(
        &mut self,
        partial_frames: Option<usize>,
        emit: &mut F,
    ) -> Result<(), PcmError>
    where
        F: for<'a> FnMut(PcmBlock<'a>),
    {
        let input = InterleavedSlice::new(&self.input, CHANNELS, RESAMPLE_CHUNK_FRAMES)
            .map_err(|source| PcmError::Buffer { source })?;
        let output_frames = self.output.len() / CHANNELS;
        let mut output = InterleavedSlice::new_mut(&mut self.output, CHANNELS, output_frames)
            .map_err(|source| PcmError::Buffer { source })?;
        let indexing = partial_frames.map(|frames| Indexing::new().partial_len(frames));
        let (_, written) = self
            .resampler
            .as_mut()
            .ok_or(PcmError::IncompleteDrain)?
            .process_into_buffer(&input, &mut output, indexing.as_ref())
            .map_err(|source| PcmError::Resample { source })?;
        self.emit_resampled(written, emit);
        Ok(())
    }

    fn emit_resampled<F>(&mut self, written: usize, emit: &mut F)
    where
        F: for<'a> FnMut(PcmBlock<'a>),
    {
        let skipped = self.delay_to_trim.min(written);
        self.delay_to_trim -= skipped;
        let available = written - skipped;
        let remaining = self.target_frames().saturating_sub(self.emitted_frames);
        let frames = available.min(usize::try_from(remaining).unwrap_or(usize::MAX));
        if frames == 0 {
            return;
        }

        let start = skipped * CHANNELS;
        let end = start + frames * CHANNELS;
        let position = add_frames(self.base_position, u128::from(self.emitted_frames), SAMPLE_RATE);
        self.emitted_frames =
            self.emitted_frames.saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        emit(PcmBlock {
            samples: &self.output[start..end],
            format: OUTPUT_FORMAT,
            frames,
            position,
        });
    }

    fn target_frames(&self) -> u64 {
        let Some(format) = self.format else { return 0 };
        let numerator = u128::from(self.real_input_frames) * u128::from(SAMPLE_RATE);
        let source_rate = u128::from(format.sample_rate);
        let frames = numerator.div_ceil(source_rate);
        u64::try_from(frames).unwrap_or(u64::MAX)
    }
}
