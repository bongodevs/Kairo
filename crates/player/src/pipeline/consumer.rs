use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use kairo_audio::mix::TapeRuntime;

use super::{
    BufferError, BufferRead, BufferedFrame, FrameMeta, NO_POSITION, Queued, State, decrement,
};
use crate::audio::FRAME_SAMPLES;
use crate::decode::Millis;
use crate::effects::{TapeCurve, TapeOptions};

pub struct FrameConsumer {
    receiver: mpsc::Receiver<Queued>,
    state: Arc<State>,
    pending: Option<Queued>,
    reached_limit: Option<u64>,
    generation: u64,
    track_epoch: u64,
    reset_epoch: u64,
    progress: u64,
    produced: bool,
    stuck_sent: bool,
    stalled_since: Option<Instant>,
    tape: Option<TapeRuntime>,
    tape_options: TapeOptions,
    tape_epoch: u64,
    tape_enabled: bool,
    last_paused: bool,
}

enum Taken {
    Frame(u64, Box<BufferedFrame>),
    End,
    Empty,
    Closed,
}

impl FrameConsumer {
    pub(super) fn new(receiver: mpsc::Receiver<Queued>, state: Arc<State>) -> Self {
        Self {
            receiver,
            state,
            pending: None,
            reached_limit: None,
            generation: 0,
            track_epoch: 0,
            reset_epoch: 0,
            progress: 0,
            produced: false,
            stuck_sent: false,
            stalled_since: None,
            tape: None,
            tape_options: TapeOptions { duration_ms: 0, curve: TapeCurve::SCurve },
            tape_epoch: 0,
            tape_enabled: false,
            last_paused: false,
        }
    }

    pub fn read(&mut self, output: &mut [u8]) -> Result<BufferRead, BufferError> {
        self.state.note_request();
        self.sync_tape();
        let paused = self.state.paused.load(Ordering::Acquire);
        if paused {
            if let Some(read) = self.read_tape(output)? {
                return Ok(read);
            }
            self.stalled_since = None;
            return Ok(BufferRead::Empty);
        }
        if self.tape_is_ramping() {
            if let Some(read) = self.read_tape(output)? {
                return Ok(read);
            }
            return Ok(BufferRead::Empty);
        }
        self.read_normal(output)
    }

    pub fn provide(&mut self, output: &mut [u8]) -> Option<usize> {
        match self.read(output) {
            Ok(BufferRead::Frame(meta)) => Some(usize::from(meta.packet_length)),
            _ => None,
        }
    }

    pub fn close(&mut self) {
        self.receiver.close();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.state.depth.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        let playing = self.state.playing.load(Ordering::Acquire);
        if !playing {
            return false;
        }
        if !self.state.paused.load(Ordering::Acquire) {
            return true;
        }
        self.tape.as_ref().is_some_and(|runtime| !runtime.is_silent())
    }

    fn read_normal(&mut self, output: &mut [u8]) -> Result<BufferRead, BufferError> {
        match self.take_next()? {
            Taken::Empty => Ok(BufferRead::Empty),
            Taken::End => Ok(BufferRead::End),
            Taken::Closed => Ok(BufferRead::Closed),
            Taken::Frame(generation, frame) => {
                let volume = frame.volume();
                let next_position = frame.next_position();
                let packet = frame.packet();
                if output.len() < packet.len() {
                    let required = packet.len();
                    self.pending = Some(Queued { generation, frame: Some(*frame) });
                    return Err(BufferError::OutputTooSmall { required, available: output.len() });
                }
                output[..packet.len()].copy_from_slice(packet);
                let meta = frame.meta();
                self.state.output_volume.store(volume.level(), Ordering::Relaxed);
                self.state.output_position.store(meta.position.get(), Ordering::Release);
                self.state.next_output_position.store(next_position.get(), Ordering::Release);
                self.produced = true;
                self.stalled_since = None;
                self.progress = self.state.worker_progress.load(Ordering::Acquire);
                decrement(&self.state.depth);
                Ok(BufferRead::Frame(meta))
            }
        }
    }

    fn read_tape(&mut self, output: &mut [u8]) -> Result<Option<BufferRead>, BufferError> {
        loop {
            let Some(emitted) = self.tape.as_mut().map(|runtime| runtime.try_emit(output)) else {
                return Ok(None);
            };
            match emitted {
                Ok(Some((length, position, volume))) => {
                    let meta = FrameMeta {
                        position,
                        pcm_frames: FRAME_SAMPLES as u16,
                        packet_length: length as u16,
                    };
                    self.state.output_volume.store(volume.level(), Ordering::Relaxed);
                    self.state.output_position.store(position.get(), Ordering::Release);
                    self.state
                        .next_output_position
                        .store(position.get().saturating_add(20), Ordering::Release);
                    self.produced = true;
                    self.stalled_since = None;
                    self.progress = self.state.worker_progress.load(Ordering::Acquire);
                    if self.tape.as_ref().is_some_and(TapeRuntime::is_inactive) {
                        self.tape = None;
                    }
                    return Ok(Some(BufferRead::Frame(meta)));
                }
                Ok(None) => {
                    let silent = self.tape.as_ref().is_some_and(TapeRuntime::is_silent);
                    if silent {
                        return Ok(None);
                    }
                    match self.take_next()? {
                        Taken::Frame(_, frame) => {
                            let volume = frame.volume();
                            if let Some(runtime) = self.tape.as_mut()
                                && runtime.push_packet(frame.packet(), volume).is_err()
                            {
                                self.tape = None;
                                return Ok(None);
                            }
                            decrement(&self.state.depth);
                        }
                        Taken::End => return Ok(Some(BufferRead::End)),
                        Taken::Closed => return Ok(Some(BufferRead::Closed)),
                        Taken::Empty => {
                            if self.state.paused.load(Ordering::Acquire) {
                                let position = self.tape_position();
                                if let Some(runtime) = self.tape.as_mut() {
                                    runtime.snap_paused(position);
                                }
                            }
                            return Ok(None);
                        }
                    }
                }
                Err(_) => {
                    self.tape = None;
                    return Ok(None);
                }
            }
        }
    }

    fn take_next(&mut self) -> Result<Taken, BufferError> {
        if let Some(generation) = self.reached_limit {
            if self.state.active.load(Ordering::Acquire) == generation {
                return Ok(Taken::Empty);
            }
            self.reached_limit = None;
        }
        loop {
            let queued = match self.pending.take() {
                Some(queued) => queued,
                None => match self.receiver.try_recv() {
                    Ok(queued) => queued,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        self.sync_stuck_state();
                        self.empty();
                        return Ok(Taken::Empty);
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => return Ok(Taken::Closed),
                },
            };
            let active = self.state.active.load(Ordering::Acquire);
            let generation = queued.generation();
            if generation < active {
                decrement(&self.state.depth);
                continue;
            }
            if generation > active {
                self.state.active.fetch_max(generation, Ordering::AcqRel);
            }
            self.sync_generation(generation);

            match queued.frame {
                Some(frame) => {
                    let end_time = self.state.end_time.load(Ordering::Acquire);
                    if end_time != NO_POSITION && frame.meta().position.get() >= end_time {
                        decrement(&self.state.depth);
                        self.reached_limit = Some(self.state.active.load(Ordering::Acquire));
                        self.mark_end();
                        return Ok(Taken::End);
                    }
                    return Ok(Taken::Frame(generation, Box::new(frame)));
                }
                None => {
                    decrement(&self.state.depth);
                    self.mark_end();
                    return Ok(Taken::End);
                }
            }
        }
    }

    fn sync_tape(&mut self) {
        let enabled = self.state.tape_enabled.load(Ordering::Acquire);
        let epoch = self.state.tape_epoch.load(Ordering::Acquire);
        if epoch != self.tape_epoch {
            self.tape_epoch = epoch;
            self.tape_options = TapeOptions {
                duration_ms: self.state.tape_duration_ms.load(Ordering::Relaxed),
                curve: TapeCurve::from_code(self.state.tape_curve.load(Ordering::Relaxed)),
            };
            self.tape_enabled = enabled;
            match self.tape.as_mut() {
                Some(runtime) => runtime.set_options(self.tape_options),
                None if enabled && self.state.paused.load(Ordering::Acquire) => {
                    let position = self.tape_position();
                    if let Ok(mut runtime) = TapeRuntime::new(self.tape_options, position) {
                        runtime.snap_paused(position);
                        self.tape = Some(runtime);
                    }
                }
                None => {}
            }
            if !enabled {
                self.tape = None;
            }
        }

        let paused = self.state.paused.load(Ordering::Acquire);
        if paused != self.last_paused {
            self.last_paused = paused;
            if paused {
                self.begin_pause();
            } else if let Some(runtime) = self.tape.as_mut() {
                runtime.begin_resume();
            }
        }
    }

    fn begin_pause(&mut self) {
        if !self.tape_enabled {
            return;
        }
        let position = self.tape_position();
        if let Some(runtime) = self.tape.as_mut() {
            runtime.begin_pause();
            return;
        }
        if let Ok(mut runtime) = TapeRuntime::new(self.tape_options, position) {
            runtime.begin_pause();
            self.tape = Some(runtime);
        }
    }

    fn tape_is_ramping(&self) -> bool {
        self.tape.as_ref().is_some_and(|runtime| !runtime.is_inactive())
    }

    fn tape_position(&self) -> Millis {
        let position = self.state.next_output_position.load(Ordering::Acquire);
        if position != NO_POSITION {
            return Millis::new(position);
        }
        let position = self.state.output_position.load(Ordering::Acquire);
        if position != NO_POSITION {
            return Millis::new(position.saturating_add(20));
        }
        Millis::default()
    }

    fn mark_end(&self) {
        self.state.playing.store(false, Ordering::Release);
        self.state.discontinuity.store(false, Ordering::Release);
        self.state.stuck_reset.fetch_add(1, Ordering::AcqRel);
        self.state.end_sequence.fetch_add(1, Ordering::AcqRel);
        self.state.end_notify.notify_one();
    }

    fn sync_stuck_state(&mut self) {
        let track_epoch = self.state.track_epoch.load(Ordering::Acquire);
        if track_epoch != self.track_epoch {
            self.track_epoch = track_epoch;
            self.produced = false;
            self.stuck_sent = false;
            self.stalled_since = None;
            self.progress = self.state.worker_progress.load(Ordering::Acquire);
            self.reset_tape();
        }
        let reset_epoch = self.state.stuck_reset.load(Ordering::Acquire);
        if reset_epoch != self.reset_epoch {
            self.reset_epoch = reset_epoch;
            self.stalled_since = None;
            self.progress = self.state.worker_progress.load(Ordering::Acquire);
        }
    }

    fn reset_tape(&mut self) {
        let position = self.tape_position();
        if let Some(runtime) = self.tape.as_mut() {
            runtime.reset(position);
        }
    }

    fn sync_generation(&mut self, generation: u64) {
        if generation == self.generation {
            return;
        }
        self.generation = generation;
        let track_epoch = self.state.track_epoch.load(Ordering::Acquire);
        if track_epoch != self.track_epoch {
            self.track_epoch = track_epoch;
            self.stuck_sent = false;
        }
        self.produced = false;
        self.stalled_since = None;
        self.progress = self.state.worker_progress.load(Ordering::Acquire);
    }

    fn empty(&mut self) {
        if !self.state.playing.load(Ordering::Acquire)
            || self.state.discontinuity.load(Ordering::Acquire)
            || !self.produced
            || self.stuck_sent
        {
            self.stalled_since = None;
            return;
        }
        let progress = self.state.worker_progress.load(Ordering::Acquire);
        if progress != self.progress {
            self.progress = progress;
            self.stalled_since = None;
            return;
        }
        let now = Instant::now();
        let stalled_since = self.stalled_since.get_or_insert(now);
        let threshold_ms = self.state.stuck_threshold_ms.load(Ordering::Acquire);
        if now.duration_since(*stalled_since) >= Duration::from_millis(threshold_ms) {
            self.stuck_sent = true;
            self.state.stuck_sequence.fetch_add(1, Ordering::AcqRel);
            self.state.stuck_notify.notify_one();
        }
    }
}
