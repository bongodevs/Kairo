//! Worker state for one active track.

use std::collections::VecDeque;
use std::sync::Arc;

use tokio::task::JoinHandle;

use kairo_audio::decode::Packet;

use super::crossfade::{ActiveFade, Armed, FadeMode, Successor};
use super::{WorkerEnd, WorkerSignal};
use crate::audio::CHANNELS;
use crate::config::PlayerConfig;
use crate::decode::{AudioBlock, BlockingDecoder, Millis, PcmBlock};
use crate::error::PlayerError;
use crate::event::TrackEndReason;
use crate::filters::Filters;
use crate::opus::{EncodedFrame, OpusFrameEncoder, OpusRoute};
use crate::pcm::{OUTPUT_FORMAT, PcmNormalizer};
use crate::pipeline::{BufferedFrame, FrameControl, FrameProducer};
use crate::track::transition::Promotion;
use crate::volume::Volume;
const PENDING_FRAMES: usize = 16;

pub(crate) enum Playback {
    Empty,
    Idle(FrameProducer),
    Running { signal: Arc<WorkerSignal>, join: JoinHandle<WorkerEnd> },
    Paused(Box<WorkerState>),
    Draining { state: Box<WorkerState>, reason: TrackEndReason },
}

pub(crate) struct WorkerState {
    pub(super) decoder: BlockingDecoder,
    pub(super) normalizer: PcmNormalizer,
    pub(super) quality: crate::pcm::ResamplingQuality,
    pub(super) encoder: OpusFrameEncoder,
    pub(super) producer: FrameProducer,
    pub(super) pending: VecDeque<BufferedFrame>,
    pub(super) ending: bool,
    pub(super) seek_ghosting: bool,
    pub(super) successor: Option<Successor>,
    pub(super) armed: Option<Armed>,
    pub(super) fade: Option<ActiveFade>,
    pub(super) base_pcm: VecDeque<f32>,
    pub(super) base_pcm_position: Millis,
    pub(super) base_eof: bool,
    pub(super) promotion: Option<Promotion>,
}

impl WorkerState {
    pub(crate) fn new(
        decoder: BlockingDecoder,
        mut encoder: OpusFrameEncoder,
        producer: FrameProducer,
        config: PlayerConfig,
        volume: Volume,
        filters: Filters,
    ) -> Result<Self, (PlayerError, FrameProducer)> {
        encoder.set_volume_immediate(volume);
        if let Err(error) = encoder.set_filters(filters) {
            return Err((error.into(), producer));
        }
        Ok(Self {
            decoder,
            normalizer: PcmNormalizer::new(config.resampling_quality),
            quality: config.resampling_quality,
            encoder,
            producer,
            pending: VecDeque::with_capacity(PENDING_FRAMES),
            ending: false,
            seek_ghosting: config.use_seek_ghosting,
            successor: None,
            armed: None,
            fade: None,
            base_pcm: VecDeque::new(),
            base_pcm_position: Millis::default(),
            base_eof: false,
            promotion: None,
        })
    }

    pub(crate) fn producer_mut(&mut self) -> &mut FrameProducer {
        &mut self.producer
    }

    pub(crate) fn buffer_control(&self) -> FrameControl {
        self.producer.control()
    }

    pub(crate) fn is_seekable(&self) -> bool {
        self.decoder.info().seekable
    }

    pub(crate) fn base_length(&self) -> Option<Millis> {
        self.decoder.info().duration
    }

    pub(crate) fn has_received_frames(&self) -> bool {
        self.producer.has_received_frames()
    }

    pub(crate) fn into_producer(self) -> FrameProducer {
        self.producer
    }

    pub(crate) fn seek(&mut self, position: Millis) -> Result<bool, PlayerError> {
        if !self.decoder.seek(position)? {
            return Ok(false);
        }
        self.normalizer.reset(position);
        self.encoder.reset(position);
        self.pending.clear();
        self.ending = false;
        self.clear_transition_state();
        if self.seek_ghosting {
            self.producer.clear_on_insert();
        } else {
            self.producer.clear_now();
            self.producer.awaiting_frame();
        }
        Ok(true)
    }

    /// Replace the decoder at a frame boundary. The encoder, volume and filters stay.
    pub(crate) fn replace_media(&mut self, decoder: BlockingDecoder, position: Millis) {
        self.decoder = decoder;
        self.normalizer.reset(position);
        self.encoder.reset(position);
        self.pending.clear();
        self.ending = false;
        self.clear_transition_state();
        // Keep the last reported position; the new timeline starts at the same millisecond.
        self.producer.clear_now_preserving_output();
        self.producer.awaiting_frame();
    }

    pub(crate) fn set_volume(&mut self, volume: Volume) {
        self.encoder.set_volume(volume);
    }

    pub(crate) fn transition_volume(&mut self, from: Volume, target: Volume) {
        self.encoder.transition_volume(from, target);
    }

    pub(crate) fn set_filters(&mut self, filters: Filters) -> Result<(), PlayerError> {
        self.encoder.set_filters(filters).map_err(Into::into)
    }

    /// Prepare a successor decoder for a transition. `fade_start` is `None` for a gapless
    pub(super) fn read_packet(&mut self) -> Result<Option<Packet>, PlayerError> {
        self.decoder.read_packet().map_err(Into::into)
    }

    pub(super) fn process_packet(&mut self, packet: Option<Packet>) -> Result<(), PlayerError> {
        if self.fade.is_none() {
            self.drain_base_pcm()?;
        }
        let Some(packet) = packet else {
            return self.finish();
        };
        let position = self.decoder.packet_position(&packet);
        let mixing = self.fade.is_some();
        let start_fade = !mixing
            && self
                .armed
                .as_ref()
                .and_then(|armed| armed.fade_start)
                .is_some_and(|start| position >= start);
        if start_fade {
            self.begin_fade(position);
        }
        let force_decode = mixing || start_fade;
        let route = if force_decode { OpusRoute::decode() } else { self.route() };
        let Some(audio) = self.decoder.decode_packet(packet, route)? else {
            return Ok(());
        };
        let block = self.decoder.audio_block(audio)?;

        match block {
            AudioBlock::Opus(block) => {
                queue_frame(&mut self.pending, BufferedFrame::from_passthrough(&block)?)?;
            }
            AudioBlock::Pcm(block) => {
                if force_decode {
                    if self.base_pcm.is_empty() {
                        self.base_pcm_position = block.position;
                    }
                    let base = &mut self.base_pcm;
                    self.normalizer.process(block, |block| {
                        base.extend(block.samples.iter().copied());
                    })?;
                    self.pump_fade()?;
                } else {
                    Self::process_input(
                        &mut self.normalizer,
                        &mut self.encoder,
                        &mut self.pending,
                        block,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn route(&self) -> OpusRoute {
        OpusRoute {
            volume: self.encoder.route_volume(),
            filters_active: self.encoder.has_pending_pcm() || self.encoder.filters_active(),
        }
    }

    fn begin_fade(&mut self, position: Millis) {
        let Some(armed) = self.armed.as_mut() else {
            return;
        };
        armed.fade_start = None;
        let total_frames = armed.duration_ms.saturating_mul(48).max(1);
        self.fade = Some(ActiveFade {
            total_frames,
            done_frames: 0,
            curve: armed.curve,
            mode: FadeMode::Mix,
            position,
        });
    }

    fn process_input(
        normalizer: &mut PcmNormalizer,
        encoder: &mut OpusFrameEncoder,
        pending: &mut VecDeque<BufferedFrame>,
        block: PcmBlock<'_>,
    ) -> Result<(), PlayerError> {
        let mut error = None;
        normalizer.process(block, |block| {
            if error.is_some() {
                return;
            }
            if let Err(source) = encoder.process(block, |frame| {
                if let Err(source) = queue_encoded(pending, &frame) {
                    error = Some(source);
                }
            }) {
                error = Some(PlayerError::Opus(source));
            }
        })?;
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(super) fn feed_pcm(
        &mut self,
        samples: &[f32],
        position: Millis,
        frames: usize,
    ) -> Result<(), PlayerError> {
        let mut error = None;
        let encoder = &mut self.encoder;
        let pending = &mut self.pending;
        encoder.process(
            PcmBlock { samples, format: OUTPUT_FORMAT, frames, position },
            |frame| {
                if let Err(source) = queue_encoded(pending, &frame) {
                    error = Some(source);
                }
            },
        )?;
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn drain_base_pcm(&mut self) -> Result<(), PlayerError> {
        if self.base_pcm.is_empty() {
            return Ok(());
        }
        let samples = self.base_pcm.drain(..).collect::<Vec<_>>();
        let frames = samples.len() / CHANNELS;
        let position = self.base_pcm_position;
        self.base_pcm_position =
            Millis::new(position.get().saturating_add(frames_to_millis(frames)));
        self.feed_pcm(&samples, position, frames)
    }
    pub(super) fn finish_encoder(&mut self) -> Result<(), PlayerError> {
        let mut error = None;
        let pending = &mut self.pending;
        self.encoder.finish(|frame| {
            if let Err(source) = queue_encoded(pending, &frame) {
                error = Some(source);
            }
        })?;
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn frames_to_millis(frames: usize) -> u64 {
    (frames as u128 * 1_000 / u128::from(crate::audio::SAMPLE_RATE)) as u64
}

fn queue_encoded(
    pending: &mut VecDeque<BufferedFrame>,
    frame: &EncodedFrame<'_>,
) -> Result<(), PlayerError> {
    queue_frame(pending, BufferedFrame::from_encoded(frame)?)
}

fn queue_frame(
    pending: &mut VecDeque<BufferedFrame>,
    frame: BufferedFrame,
) -> Result<(), PlayerError> {
    if pending.len() == PENDING_FRAMES {
        return Err(PlayerError::WorkerBurst);
    }
    pending.push_back(frame);
    Ok(())
}
