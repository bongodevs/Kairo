//! Crossfade, gapless, and successor promotion inside the worker.

use std::collections::VecDeque;

use super::worker::WorkerState;
use crate::audio::{CHANNELS, FRAME_SAMPLES};
use crate::decode::{AudioBlock, BlockingDecoder, Millis};
use crate::effects::{CrossfadeCurve, CrossfadeOptions};
use crate::error::PlayerError;
use crate::event::TrackEndReason;
use crate::opus::OpusRoute;
use crate::pcm::PcmNormalizer;
use crate::track::transition::Promotion;
const FRAME_MS: u64 = 20;
const RECOVER_FRAMES: u64 = 150 * 48;

pub(super) struct Successor {
    decoder: BlockingDecoder,
    normalizer: PcmNormalizer,
    pcm: VecDeque<f32>,
    position: Millis,
    eof: bool,
}

impl Successor {
    pub(super) fn new(
        decoder: BlockingDecoder,
        quality: crate::pcm::ResamplingQuality,
        position: Millis,
    ) -> Self {
        Self {
            decoder,
            normalizer: PcmNormalizer::new(quality),
            pcm: VecDeque::new(),
            position,
            eof: false,
        }
    }
}

pub(super) struct Armed {
    pub(super) fade_start: Option<Millis>,
    pub(super) duration_ms: u64,
    pub(super) curve: CrossfadeCurve,
    pub(super) gapless: bool,
}

#[derive(Clone, Copy)]
pub(super) enum FadeMode {
    Mix,
    Recover { start_gain: f64 },
}

pub(super) struct ActiveFade {
    pub(super) total_frames: u64,
    pub(super) done_frames: u64,
    pub(super) curve: CrossfadeCurve,
    pub(super) mode: FadeMode,
    pub(super) position: Millis,
}

impl WorkerState {
    pub(crate) fn prepare_next(
        &mut self,
        decoder: BlockingDecoder,
        position: Millis,
        options: CrossfadeOptions,
        fade_start: Option<Millis>,
    ) {
        self.successor = Some(Successor::new(decoder, self.quality, position));
        self.armed = Some(Armed {
            fade_start,
            duration_ms: options.duration_ms,
            curve: options.curve,
            gapless: options.duration_ms == 0 && options.gapless,
        });
        self.fade = None;
        self.base_pcm.clear();
        self.base_eof = false;
    }

    pub(crate) fn rearm_next(&mut self, options: CrossfadeOptions, fade_start: Option<Millis>) {
        if self.successor.is_none() || self.fade.is_some() {
            return;
        }
        self.armed = Some(Armed {
            fade_start,
            duration_ms: options.duration_ms,
            curve: options.curve,
            gapless: options.duration_ms == 0 && options.gapless,
        });
    }

    pub(crate) fn clear_next(&mut self) {
        if self.fade.is_some() {
            let position = self.fade.as_ref().map_or(self.base_pcm_position, |fade| fade.position);
            self.recover_fade(position);
            return;
        }
        self.successor = None;
        self.armed = None;
        self.base_pcm.clear();
        self.base_eof = false;
    }

    pub(crate) fn clear_transition_state(&mut self) {
        self.successor = None;
        self.armed = None;
        self.fade = None;
        self.base_pcm.clear();
        self.base_pcm_position = Millis::default();
        self.base_eof = false;
        self.promotion = None;
    }

    /// Start the prepared successor at the next decoded packet. A fade uses the manual overlap,
    /// while a gapless successor is promoted at the next hand-off.
    pub(crate) fn transition_now(&mut self, options: CrossfadeOptions) -> bool {
        if self.successor.is_none() {
            return false;
        }
        if self.fade.is_some() {
            return true;
        }
        if options.duration_ms > 0 {
            self.armed = Some(Armed {
                fade_start: Some(Millis::default()),
                duration_ms: options.manual_duration_ms,
                curve: options.curve,
                gapless: false,
            });
            true
        } else if options.gapless {
            self.armed = Some(Armed {
                fade_start: None,
                duration_ms: 0,
                curve: options.curve,
                gapless: true,
            });
            true
        } else {
            false
        }
    }

    pub(crate) fn has_successor(&self) -> bool {
        self.successor.is_some()
    }

    pub(crate) fn transition_reason(&self) -> TrackEndReason {
        if self.armed.as_ref().is_some_and(|armed| armed.gapless) {
            TrackEndReason::Gapless
        } else {
            TrackEndReason::Crossfade
        }
    }

    pub(crate) fn take_promotion(&mut self) -> Option<Promotion> {
        self.promotion.take()
    }

    /// End the base at an end-time or ordinary EOF while a successor is held.
    pub(super) fn finish_transition(&mut self, reason: TrackEndReason) -> Result<(), PlayerError> {
        if self.fade.is_some() {
            self.base_eof = true;
            self.pump_fade()?;
        }
        if self.successor.is_some() {
            self.promote(reason);
        }
        if self.successor.is_none() && self.fade.is_none() && self.promotion.is_none() {
            self.finish_encoder()?;
            self.ending = true;
        }
        Ok(())
    }
    fn pull_successor(&mut self, need: usize) -> Result<(), PlayerError> {
        loop {
            let Some(successor) = self.successor.as_mut() else {
                return Ok(());
            };
            if successor.eof || successor.pcm.len() / CHANNELS >= need {
                return Ok(());
            }
            let Successor { decoder, normalizer, pcm, position, eof } = successor;
            let Some(packet) = decoder.read_packet()? else {
                normalizer.finish(|block| {
                    if pcm.is_empty() {
                        *position = block.position;
                    }
                    pcm.extend(block.samples.iter().copied());
                })?;
                *eof = true;
                continue;
            };
            let Some(audio) = decoder.decode_packet(packet, OpusRoute::decode())? else {
                continue;
            };
            let block = decoder.audio_block(audio)?;
            if let AudioBlock::Pcm(block) = block {
                if pcm.is_empty() {
                    *position = block.position;
                }
                normalizer.process(block, |block| {
                    pcm.extend(block.samples.iter().copied());
                })?;
            }
        }
    }

    pub(super) fn pump_fade(&mut self) -> Result<(), PlayerError> {
        loop {
            let Some(fade) = self.fade.as_ref() else {
                return Ok(());
            };
            let mixing = matches!(fade.mode, FadeMode::Mix);
            let total_frames = fade.total_frames.max(1);
            let done_frames = fade.done_frames;
            let curve = fade.curve;
            let mode = fade.mode;
            let position = fade.position;
            let base_frames = self.base_pcm.len() / CHANNELS;

            if mixing {
                let next_frames =
                    self.successor.as_ref().map_or(0, |next| next.pcm.len() / CHANNELS);
                if next_frames < FRAME_SAMPLES {
                    if self.successor.as_ref().is_some_and(|next| next.eof) {
                        self.recover_fade(position);
                        continue;
                    }
                    if self.pull_successor(FRAME_SAMPLES).is_err() {
                        self.recover_fade(position);
                        continue;
                    }
                    continue;
                }
                if base_frames < FRAME_SAMPLES && !self.base_eof {
                    return Ok(());
                }
            } else if base_frames == 0 {
                if self.base_eof {
                    self.fade = None;
                    return Ok(());
                }
                return Ok(());
            }

            let mut mixed = [0.0f32; FRAME_SAMPLES * CHANNELS];
            for index in 0..FRAME_SAMPLES {
                let progress =
                    ((done_frames + index as u64) as f64 / total_frames as f64).clamp(0.0, 1.0);
                let (base_left, base_right) = pop_pair(&mut self.base_pcm);
                let sample = match mode {
                    FadeMode::Mix => {
                        let (next_left, next_right) = self
                            .successor
                            .as_mut()
                            .map_or((0.0, 0.0), |next| pop_pair(&mut next.pcm));
                        let outgoing = curve.ease_out(progress) as f32;
                        let incoming = curve.ease(progress) as f32;
                        (
                            base_left * outgoing + next_left * incoming,
                            base_right * outgoing + next_right * incoming,
                        )
                    }
                    FadeMode::Recover { start_gain } => {
                        let gain = (start_gain + (1.0 - start_gain) * progress) as f32;
                        (base_left * gain, base_right * gain)
                    }
                };
                let target = index * CHANNELS;
                mixed[target] = sample.0;
                mixed[target + 1] = sample.1;
            }
            self.feed_pcm(&mixed, position, FRAME_SAMPLES)?;
            if matches!(mode, FadeMode::Mix)
                && let Some(next) = self.successor.as_mut()
            {
                next.position = Millis::new(next.position.get().saturating_add(FRAME_MS));
            }

            let fade = self.fade.as_mut().expect("fade still active");
            fade.done_frames += FRAME_SAMPLES as u64;
            fade.position = Millis::new(position.get().saturating_add(FRAME_MS));
            if fade.done_frames >= fade.total_frames {
                match fade.mode {
                    FadeMode::Mix => self.promote(TrackEndReason::Crossfade),
                    FadeMode::Recover { .. } => self.fade = None,
                }
            }
        }
    }

    fn recover_fade(&mut self, position: Millis) {
        let start_gain = self.fade.take().map_or(1.0, |fade| {
            let progress = fade.done_frames as f64 / fade.total_frames.max(1) as f64;
            match fade.mode {
                FadeMode::Mix => fade.curve.ease_out(progress),
                FadeMode::Recover { start_gain } => start_gain,
            }
        });
        if start_gain < 0.999 {
            self.fade = Some(ActiveFade {
                total_frames: RECOVER_FRAMES.max(1),
                done_frames: 0,
                curve: CrossfadeCurve::Linear,
                mode: FadeMode::Recover { start_gain },
                position,
            });
        }
        self.successor = None;
        self.armed = None;
    }

    fn promote(&mut self, reason: TrackEndReason) {
        let Some(mut successor) = self.successor.take() else {
            return;
        };
        let position = successor.position;
        let leftover = std::mem::take(&mut successor.pcm);
        let decoder = std::mem::replace(&mut self.decoder, successor.decoder);
        let normalizer = std::mem::replace(&mut self.normalizer, successor.normalizer);
        drop(decoder);
        drop(normalizer);
        self.base_pcm = leftover;
        self.base_pcm_position = position;
        self.base_eof = successor.eof;
        self.fade = None;
        self.armed = None;
        self.encoder.rebase(position);
        self.producer.track_boundary();
        self.promotion = Some(Promotion { position, reason });
    }

    pub(super) fn finish(&mut self) -> Result<(), PlayerError> {
        let mut tail = Vec::new();
        let mut tail_position = Millis::default();
        let mut first = true;
        self.normalizer.finish(|block| {
            if first {
                tail_position = block.position;
                first = false;
            }
            tail.extend(block.samples.iter().copied());
        })?;

        if self.fade.is_some() {
            if !tail.is_empty() && self.base_pcm.is_empty() {
                self.base_pcm_position = tail_position;
            }
            self.base_pcm.extend(tail);
            self.base_eof = true;
            self.pump_fade()?;
            if self.successor.is_none() && self.fade.is_none() && self.promotion.is_none() {
                self.finish_encoder()?;
                self.ending = true;
            }
            return Ok(());
        }

        if self.successor.is_some() {
            if !tail.is_empty() {
                let frames = tail.len() / CHANNELS;
                self.feed_pcm(&tail, tail_position, frames)?;
            }
            let reason = self.transition_reason();
            self.promote(reason);
            return Ok(());
        }

        if !tail.is_empty() {
            let frames = tail.len() / CHANNELS;
            self.feed_pcm(&tail, tail_position, frames)?;
        }
        self.finish_encoder()?;
        self.ending = true;
        Ok(())
    }
}
fn pop_pair(queue: &mut VecDeque<f32>) -> (f32, f32) {
    let left = queue.pop_front().unwrap_or(0.0);
    let right = queue.pop_front().unwrap_or(0.0);
    (left, right)
}
