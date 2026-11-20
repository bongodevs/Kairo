#[cfg(test)]
#[path = "../tests/opus_encoder.rs"]
mod encoder_tests;
#[cfg(test)]
#[path = "../tests/opus_filter_contract.rs"]
mod filter_contract_tests;
#[cfg(test)]
#[path = "../tests/opus_pipeline.rs"]
mod pipeline_tests;
#[cfg(test)]
#[path = "../tests/opus_timescale.rs"]
mod timescale_tests;

use opus_rs::{Application, OpusEncoder as CodecEncoder};

use kairo_core::Millis;

use crate::clock::add_frames;
use crate::decode::PcmBlock;
use crate::filters::{
    ChannelMix, CutoffLowPass, Distortion, Echo, Equalizer, EqualizerConfig, FilterVolume, Filters,
    HighPass, Karaoke, LowPass, Normalization, Rotation, Timescale, TimescaleConfig, Tremolo,
    Vibrato,
};
use crate::mix::{Volume, VolumeRamp};
use crate::opus::{
    CHANNELS, EncodedFrame, FRAME_SAMPLES, MAX_PACKET_BYTES, OpusEncoderConfig, OpusError,
    OpusStats, SAMPLE_RATE,
};
use crate::pcm::OUTPUT_FORMAT;

pub(super) struct TimescaleOutput {
    filter: Timescale,
    pcm: Box<[f32]>,
    pub(super) frames: usize,
}

impl TimescaleOutput {
    pub(super) fn new(config: TimescaleConfig) -> Result<Self, OpusError> {
        Ok(Self {
            filter: Timescale::new(config)?,
            pcm: vec![0.0; FRAME_SAMPLES * CHANNELS].into_boxed_slice(),
            frames: 0,
        })
    }
}

impl OpusFrameEncoder {
    pub(super) fn process_input_frame<F>(
        &mut self,
        pcm_frames: usize,
        emit: &mut F,
    ) -> Result<(), OpusError>
    where
        F: for<'a> FnMut(EncodedFrame<'a>),
    {
        let sample_count = pcm_frames * CHANNELS;
        let mut pcm = std::mem::take(&mut self.pcm);
        self.process_pre_timescale(&mut pcm[..sample_count]);
        let result = match self.timescale.take() {
            Some(mut output) => {
                let result = self.process_timescale(&mut output, &pcm[..sample_count], emit);
                self.timescale = Some(output);
                result
            }
            None => self.encode_samples(&mut pcm, pcm_frames, emit),
        };
        self.pcm = pcm;
        result
    }

    /// Filters that run before timescale in signal order.
    fn process_pre_timescale(&mut self, samples: &mut [f32]) {
        if let Some(high_pass) = self.high_pass.as_mut() {
            high_pass.process(samples);
        }
        if let Some(low_pass) = self.cutoff_low_pass.as_mut() {
            low_pass.process(samples);
        }
        if let Some(normalization) = self.normalization.as_mut() {
            normalization.process(samples);
        }
        if let Some(echo) = self.echo.as_mut() {
            echo.process(samples);
        }
        if let Some(low_pass) = self.low_pass.as_mut() {
            low_pass.process(samples);
        }
        if let Some(channel_mix) = self.channel_mix.as_ref() {
            channel_mix.process(samples);
        }
        if let Some(rotation) = self.rotation.as_mut() {
            rotation.process(samples);
        }
        if let Some(distortion) = self.distortion.as_ref() {
            distortion.process(samples);
        }
        if let Some(vibrato) = self.vibrato.as_mut() {
            vibrato.process(samples);
        }
        if let Some(tremolo) = self.tremolo.as_mut() {
            tremolo.process(samples);
        }
    }

    /// Filters that run after timescale in signal order.
    fn process_post_timescale(&mut self, samples: &mut [f32]) {
        if let Some(karaoke) = self.karaoke.as_mut() {
            karaoke.process(samples);
        }
        self.equalizer.process(samples);
        if let Some(volume) = self.filter_volume.as_ref() {
            volume.process(samples);
        }
    }

    pub(super) fn finish_timescale<F>(&mut self, emit: &mut F) -> Result<(), OpusError>
    where
        F: for<'a> FnMut(EncodedFrame<'a>),
    {
        let Some(mut output) = self.timescale.take() else {
            return Ok(());
        };
        let mut error = None;
        let result = {
            let filter = &mut output.filter;
            let pcm = &mut output.pcm;
            let frames = &mut output.frames;
            filter.drain(|samples| {
                if error.is_none()
                    && let Err(source) = push_output(self, pcm, frames, samples, emit)
                {
                    error = Some(source);
                }
            })
        };
        result?;
        if let Some(source) = error {
            return Err(source);
        }
        if output.frames > 0 {
            self.encode_samples(&mut output.pcm, output.frames, emit)?;
            output.frames = 0;
        }
        self.timescale = Some(output);
        Ok(())
    }

    pub(super) fn reset_processing(&mut self) {
        self.equalizer.reset();
        if let Some(output) = self.timescale.as_mut() {
            output.filter.reset();
            output.frames = 0;
        }
        if let Some(low_pass) = self.low_pass.as_mut() {
            low_pass.reset();
        }
    }

    fn process_timescale<F>(
        &mut self,
        output: &mut TimescaleOutput,
        input: &[f32],
        emit: &mut F,
    ) -> Result<(), OpusError>
    where
        F: for<'a> FnMut(EncodedFrame<'a>),
    {
        let mut error = None;
        let result = {
            let filter = &mut output.filter;
            let pcm = &mut output.pcm;
            let frames = &mut output.frames;
            filter.process(input, |samples| {
                if error.is_none()
                    && let Err(source) = push_output(self, pcm, frames, samples, emit)
                {
                    error = Some(source);
                }
            })
        };
        result?;
        match error {
            Some(source) => Err(source),
            None => Ok(()),
        }
    }

    fn encode_samples<F>(
        &mut self,
        pcm: &mut [f32],
        pcm_frames: usize,
        emit: &mut F,
    ) -> Result<(), OpusError>
    where
        F: for<'a> FnMut(EncodedFrame<'a>),
    {
        let sample_count = pcm_frames * CHANNELS;
        let samples = &mut pcm[..sample_count];
        self.process_post_timescale(samples);
        pcm[sample_count..].fill(0.0);
        let volume = self.volume.apply_frame(pcm);
        sanitize_pcm(pcm);
        let length = self
            .encoder
            .encode(pcm, FRAME_SAMPLES, &mut self.packet)
            .map_err(|message| OpusError::Encode { message })?;
        self.stats.frames += 1;
        self.stats.bytes =
            self.stats.bytes.saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
        let position = Millis::new((self.position_micros / 1_000.0) as u64);
        let duration_micros = pcm_frames as f64 * 1_000_000.0 / crate::SAMPLE_RATE as f64;
        self.position_micros += duration_micros * self.source_speed();
        if let Some((position, leftover)) = self.rebase_after.take() {
            let played = FRAME_SAMPLES.saturating_sub(leftover).min(pcm_frames);
            let played_micros = played as f64 * 1_000_000.0 / crate::SAMPLE_RATE as f64;
            self.position_micros = position.get() as f64 * 1_000.0 + played_micros;
        }
        let next_position = Millis::new((self.position_micros / 1_000.0) as u64);
        emit(EncodedFrame {
            packet: &self.packet[..length],
            position,
            next_position,
            pcm_frames,
            volume: volume.level(),
        });
        Ok(())
    }

    fn source_speed(&self) -> f64 {
        self.source_speed
    }
}

fn push_output<F>(
    encoder: &mut OpusFrameEncoder,
    pcm: &mut [f32],
    frames: &mut usize,
    samples: &[f32],
    emit: &mut F,
) -> Result<(), OpusError>
where
    F: for<'a> FnMut(EncodedFrame<'a>),
{
    let mut offset = 0;
    while offset < samples.len() {
        let available = (FRAME_SAMPLES - *frames) * CHANNELS;
        let count = (samples.len() - offset).min(available);
        let target = *frames * CHANNELS;
        pcm[target..target + count].copy_from_slice(&samples[offset..offset + count]);
        *frames += count / CHANNELS;
        offset += count;
        if *frames == FRAME_SAMPLES {
            encoder.encode_samples(pcm, FRAME_SAMPLES, emit)?;
            *frames = 0;
        }
    }
    Ok(())
}

impl OpusFrameEncoder {
    pub fn set_volume(&mut self, volume: Volume) {
        self.volume.set(volume);
    }

    pub fn set_volume_immediate(&mut self, volume: Volume) {
        self.volume.set_immediate(volume);
    }

    pub fn transition_volume(&mut self, from: Volume, target: Volume) {
        self.volume.set_immediate(from);
        self.volume.set(target);
    }

    pub fn route_volume(&self) -> u16 {
        self.volume.route_level()
    }

    pub fn has_pending_pcm(&self) -> bool {
        self.pcm_frames > 0 || self.timescale.as_ref().is_some_and(|output| output.frames > 0)
    }

    pub fn set_filters(&mut self, filters: Filters) -> Result<(), OpusError> {
        filters.validate()?;
        let filter_volume =
            if filters.volume != 1.0 { Some(FilterVolume::new(filters.volume)?) } else { None };
        let source_speed = filters.timescale.map_or(1.0, TimescaleConfig::source_speed);
        let timescale = match filters.timescale.filter(|config| config.is_enabled()) {
            Some(config) => Some(TimescaleOutput::new(config)?),
            None => None,
        };
        let tremolo = match filters.tremolo.filter(|config| config.is_enabled()) {
            Some(config) => Some(Tremolo::new(config)?),
            None => None,
        };
        let vibrato = match filters.vibrato.filter(|config| config.is_enabled()) {
            Some(config) => Some(Vibrato::new(config)?),
            None => None,
        };
        let distortion =
            filters.distortion.filter(|config| config.is_enabled()).map(Distortion::new);
        let rotation = filters.rotation.filter(|config| config.is_enabled()).map(Rotation::new);
        let channel_mix =
            filters.channel_mix.filter(|config| config.is_enabled()).map(ChannelMix::new);
        let low_pass = filters.low_pass.filter(|config| config.is_enabled()).map(LowPass::new);
        let high_pass = filters.high_pass.filter(|config| config.is_enabled()).map(HighPass::new);
        let cutoff_low_pass =
            filters.cutoff_low_pass.filter(|config| config.is_enabled()).map(CutoffLowPass::new);
        let normalization =
            filters.normalization.filter(|config| config.is_enabled()).map(Normalization::new);
        let echo = match filters.echo.filter(|config| config.is_enabled()) {
            Some(config) => Some(Echo::new(config)?),
            None => None,
        };
        self.filter_volume = filter_volume;
        self.equalizer.set_config(filters.equalizer);
        self.karaoke = filters.karaoke.map(Karaoke::new);
        self.timescale = timescale;
        self.tremolo = tremolo;
        self.vibrato = vibrato;
        self.distortion = distortion;
        self.rotation = rotation;
        self.channel_mix = channel_mix;
        self.low_pass = low_pass;
        self.high_pass = high_pass;
        self.cutoff_low_pass = cutoff_low_pass;
        self.normalization = normalization;
        self.echo = echo;
        self.source_speed = source_speed;
        Ok(())
    }

    pub fn filters_active(&self) -> bool {
        self.filter_volume.is_some()
            || self.equalizer.is_enabled()
            || self.karaoke.is_some()
            || self.timescale.is_some()
            || self.tremolo.is_some()
            || self.vibrato.is_some()
            || self.distortion.is_some()
            || self.rotation.is_some()
            || self.channel_mix.is_some()
            || self.low_pass.is_some()
            || self.high_pass.is_some()
            || self.cutoff_low_pass.is_some()
            || self.normalization.is_some()
            || self.echo.is_some()
    }
}

/// Worker-owned 20 ms frame assembler and pure-Rust Opus encoder.
pub struct OpusFrameEncoder {
    encoder: CodecEncoder,
    pcm: Box<[f32]>,
    pcm_frames: usize,
    packet: [u8; MAX_PACKET_BYTES],
    position_micros: f64,
    position_started: bool,
    rebase_after: Option<(Millis, usize)>,
    finished: bool,
    stats: OpusStats,
    volume: VolumeRamp,
    filter_volume: Option<FilterVolume>,
    equalizer: Equalizer,
    karaoke: Option<Karaoke>,
    timescale: Option<TimescaleOutput>,
    tremolo: Option<Tremolo>,
    vibrato: Option<Vibrato>,
    distortion: Option<Distortion>,
    rotation: Option<Rotation>,
    channel_mix: Option<ChannelMix>,
    low_pass: Option<LowPass>,
    high_pass: Option<HighPass>,
    cutoff_low_pass: Option<CutoffLowPass>,
    normalization: Option<Normalization>,
    echo: Option<Echo>,
    source_speed: f64,
}

impl OpusFrameEncoder {
    pub fn new(config: OpusEncoderConfig) -> Result<Self, OpusError> {
        let config = config.validate()?;
        // Audio at 48 kHz starts fullband, and opus-rs leaves its internal DTX switch off.
        let mut encoder = CodecEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio)
            .map_err(|message| OpusError::Initialize { message })?;
        config.apply_to(&mut encoder);

        Ok(Self {
            encoder,
            pcm: vec![0.0; FRAME_SAMPLES * CHANNELS].into_boxed_slice(),
            pcm_frames: 0,
            packet: [0; MAX_PACKET_BYTES],
            position_micros: 0.0,
            position_started: false,
            rebase_after: None,
            finished: false,
            stats: OpusStats::default(),
            volume: VolumeRamp::new(Volume::default()),
            filter_volume: None,
            equalizer: Equalizer::new(EqualizerConfig::default()),
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
            source_speed: 1.0,
        })
    }

    #[must_use]
    pub fn lookahead(&self) -> usize {
        self.encoder.lookahead()
    }

    #[must_use]
    pub fn stats(&self) -> OpusStats {
        self.stats
    }

    /// Assemble and encode all complete 20 ms frames in a normalized PCM block.
    pub fn process<F>(&mut self, block: PcmBlock<'_>, mut emit: F) -> Result<(), OpusError>
    where
        F: for<'a> FnMut(EncodedFrame<'a>),
    {
        if self.finished {
            return Err(OpusError::Finished);
        }
        validate_block(&block)?;

        let mut offset = 0;
        while offset < block.frames {
            if !self.position_started {
                self.position_micros =
                    add_frames(block.position, offset as u128, SAMPLE_RATE).get() as f64 * 1_000.0;
                self.position_started = true;
            }
            let frames = (block.frames - offset).min(FRAME_SAMPLES - self.pcm_frames);
            let source = offset * CHANNELS;
            let target = self.pcm_frames * CHANNELS;
            let samples = frames * CHANNELS;
            self.pcm[target..target + samples]
                .copy_from_slice(&block.samples[source..source + samples]);
            self.pcm_frames += frames;
            offset += frames;

            if self.pcm_frames == FRAME_SAMPLES {
                self.process_input_frame(FRAME_SAMPLES, &mut emit)?;
                self.pcm_frames = 0;
            }
        }
        Ok(())
    }

    /// Zero-pad and encode one final partial frame, if any.
    pub fn finish<F>(&mut self, mut emit: F) -> Result<(), OpusError>
    where
        F: for<'a> FnMut(EncodedFrame<'a>),
    {
        if self.finished {
            return Ok(());
        }
        if self.pcm_frames > 0 {
            let real_frames = self.pcm_frames;
            self.process_input_frame(real_frames, &mut emit)?;
            self.pcm_frames = 0;
        }
        self.finish_timescale(&mut emit)?;
        self.finished = true;
        Ok(())
    }

    /// Discard a partial PCM frame after a seek while retaining codec history.
    pub fn reset(&mut self, position: Millis) {
        self.pcm_frames = 0;
        self.position_micros = position.get() as f64 * 1_000.0;
        self.position_started = true;
        self.rebase_after = None;
        self.finished = false;
        self.reset_processing();
    }

    /// Continue the current partial frame with a new track's timeline.
    pub fn rebase(&mut self, position: Millis) {
        if self.pcm_frames == 0 {
            self.position_micros = position.get() as f64 * 1_000.0;
        } else {
            self.rebase_after = Some((position, self.pcm_frames));
        }
        self.position_started = true;
    }
}

fn validate_block(block: &PcmBlock<'_>) -> Result<(), OpusError> {
    if block.format != OUTPUT_FORMAT {
        return Err(OpusError::InvalidFormat { format: block.format });
    }
    let expected = block.frames.saturating_mul(CHANNELS);
    if block.samples.len() != expected {
        return Err(OpusError::InvalidBlock { expected, actual: block.samples.len() });
    }
    Ok(())
}

/// Replace non-finite samples without changing finite PCM on the unity path.
#[inline]
pub(crate) fn sanitize_pcm(samples: &mut [f32]) {
    for sample in samples {
        if !sample.is_finite() {
            *sample = 0.0;
        }
    }
}
