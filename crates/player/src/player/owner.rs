use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::PlayerState;
use crate::config::PlayerConfig;
use crate::decode::{BlockingDecoder, Millis};
use crate::effects::{CrossfadeOptions, TapeOptions};
use crate::error::PlayerError;
use crate::event::{PlayerEvent, TrackEndReason};
use crate::filters::Filters;
use crate::pipeline::{
    FrameControl, FrameProducer, Playback, WorkerEnd, WorkerSignal, WorkerState, run_worker,
};
use crate::source::{OpenedSource, Sources};
use crate::track::Track;
use crate::track::transition::PromotionSlot;
use crate::volume::Volume;

pub(crate) struct PlayerOwner {
    pub(crate) sources: Sources,
    pub(crate) config: PlayerConfig,
    pub(crate) runtime: Handle,
    pub(crate) playback: Playback,
    pub(crate) track: Option<Track>,
    pub(crate) paused: bool,
    pub(crate) end_time: Option<Millis>,
    pub(crate) volume: Volume,
    pub(crate) filters: Filters,
    pub(crate) tape: Option<TapeOptions>,
    pub(crate) crossfade: Option<CrossfadeOptions>,
    pub(crate) next: Option<Track>,
    pub(crate) buffer: FrameControl,
    pub(crate) end_seen: u64,
    pub(crate) stuck_seen: u64,
    pub(crate) promotion: Arc<PromotionSlot>,
    pub(crate) promotion_seen: u64,
    pub(crate) events: mpsc::Sender<PlayerEvent>,
    pub(crate) cleanup_last_request: Instant,
    pub(crate) cleanup_deadline: Option<Instant>,
}

impl PlayerOwner {
    pub(crate) fn new(
        sources: Sources,
        config: PlayerConfig,
        producer: FrameProducer,
        buffer: FrameControl,
        events: mpsc::Sender<PlayerEvent>,
    ) -> Self {
        let end_seen = buffer.end_sequence();
        let stuck_seen = buffer.stuck_sequence();
        let promotion = Arc::new(PromotionSlot::new());
        let promotion_seen = promotion.sequence();
        Self {
            sources,
            config,
            runtime: Handle::current(),
            playback: Playback::Idle(producer),
            track: None,
            paused: false,
            end_time: None,
            volume: Volume::default(),
            filters: Filters::default(),
            tape: None,
            crossfade: config.crossfade,
            next: None,
            buffer,
            end_seen,
            stuck_seen,
            promotion,
            promotion_seen,
            events,
            cleanup_last_request: Instant::now(),
            cleanup_deadline: None,
        }
    }

    pub(crate) async fn pause(&mut self, paused: bool) -> Result<(), PlayerError> {
        self.settle().await?;
        if self.paused == paused {
            return Ok(());
        }
        self.paused = paused;
        let playback = self.take_playback();
        match (paused, playback) {
            (true, Playback::Running { signal, join }) => {
                signal.pause();
                let end = join.await.map_err(|_| PlayerError::WorkerGone)?;
                let result = self.store_worker_end(end).await;
                match &mut self.playback {
                    Playback::Paused(state) | Playback::Draining { state, .. } => {
                        state.producer_mut().set_paused(true);
                    }
                    _ => {}
                }
                result?;
            }
            (true, Playback::Draining { mut state, reason }) => {
                state.producer_mut().set_paused(true);
                self.playback = Playback::Draining { state, reason };
            }
            (false, Playback::Paused(mut state)) => {
                state.producer_mut().set_paused(false);
                self.launch(*state);
            }
            (false, Playback::Draining { mut state, reason }) => {
                state.producer_mut().set_paused(false);
                self.playback = Playback::Draining { state, reason };
            }
            (_, Playback::Idle(producer)) => {
                producer.set_paused(paused);
                self.playback = Playback::Idle(producer);
            }
            (_, other) => self.playback = other,
        }
        let position = self.current_position();
        if let Some(track) = self.track.as_ref().map(|track| track.snapshot(position)) {
            let event =
                if paused { PlayerEvent::Paused { track } } else { PlayerEvent::Resumed { track } };
            self.emit_event(event).await;
        }
        Ok(())
    }

    pub(crate) fn launch(&mut self, state: WorkerState) {
        state.interrupt_handle().reset();
        let signal = WorkerSignal::new(
            self.end_time,
            state.buffer_control(),
            Arc::clone(&self.promotion),
            state.interrupt_handle(),
        );
        let worker_signal = signal.clone();
        let runtime = self.runtime.clone();
        let state = Box::new(state);
        let join = tokio::task::spawn_blocking(move || run_worker(*state, worker_signal, runtime));
        self.playback = Playback::Running { signal, join };
    }

    pub(crate) fn state(&self) -> PlayerState {
        let position = self.current_position();
        PlayerState {
            track: self.track.as_ref().map(|track| track.snapshot(position)),
            position,
            paused: self.paused,
            end_time: self.end_time,
            volume: self.volume.level(),
            filters: self.filters,
        }
    }

    pub(crate) fn current_position(&self) -> Millis {
        self.buffer
            .position()
            .or_else(|| self.track.as_ref().map(Track::initial_position))
            .unwrap_or_default()
    }

    pub(crate) fn set_user_data(&mut self, user_data: Map<String, Value>) -> bool {
        let Some(track) = self.track.as_mut() else {
            return false;
        };
        track.set_user_data(user_data);
        true
    }

    pub(crate) fn take_playback(&mut self) -> Playback {
        std::mem::replace(&mut self.playback, Playback::Empty)
    }

    pub(crate) fn set_end_time(&mut self, end_time: Option<Millis>) -> Result<(), PlayerError> {
        self.end_time = end_time;
        match &mut self.playback {
            Playback::Running { signal, .. } => signal.set_end_time(end_time),
            Playback::Paused(state) => state.producer_mut().set_end_time(end_time),
            Playback::Draining { state, .. } => state.producer_mut().set_end_time(end_time),
            Playback::Idle(producer) => producer.set_end_time(end_time),
            Playback::Empty => return Err(PlayerError::WorkerGone),
        }
        Ok(())
    }
}

impl PlayerOwner {
    pub(crate) async fn set_volume(&mut self, volume: Volume) -> Result<(), PlayerError> {
        self.settle().await?;
        if self.volume == volume {
            return Ok(());
        }
        let previous = self.volume;
        self.volume = volume;
        self.reconfigure_processing(previous, move |state, seekable| {
            if !seekable {
                state.set_volume(volume);
            }
            Ok(())
        })
        .await
    }
}

impl PlayerOwner {
    pub(crate) async fn set_tape(
        &mut self,
        options: Option<crate::effects::TapeOptions>,
    ) -> Result<(), PlayerError> {
        self.settle().await?;
        if self.tape == options {
            return Ok(());
        }
        self.tape = options;
        self.buffer.set_tape(options);
        Ok(())
    }

    pub(crate) async fn set_filters(&mut self, filters: Filters) -> Result<(), PlayerError> {
        self.settle().await?;
        filters.validate().map_err(crate::opus::OpusError::from)?;
        if self.filters == filters {
            return Ok(());
        }
        self.reconfigure_processing(self.volume, move |state, _| state.set_filters(filters))
            .await?;
        self.filters = filters;
        Ok(())
    }

    pub(crate) async fn reconfigure_processing<F>(
        &mut self,
        previous_volume: Volume,
        update: F,
    ) -> Result<(), PlayerError>
    where
        F: FnOnce(&mut WorkerState, bool) -> Result<(), PlayerError> + Send,
    {
        let playback = self.take_playback();
        let (state, position, heard_volume, draining) = match playback {
            Playback::Running { signal, join } => {
                signal.set_buffer_paused(true);
                signal.pause();
                let (state, reason) = match join.await.map_err(|_| PlayerError::WorkerGone)? {
                    WorkerEnd::Paused(state) => (state, None),
                    WorkerEnd::Finished(state) => (state, Some(TrackEndReason::Finished)),
                    end @ WorkerEnd::Failed { .. } => {
                        return self.store_worker_end(end).await;
                    }
                    WorkerEnd::Stopped(state) => {
                        self.playback = Playback::Idle(state.into_producer());
                        self.track = None;
                        self.end_time = None;
                        return Ok(());
                    }
                };
                let output = signal.output_state().unwrap_or((Millis::default(), previous_volume));
                (state, output.0, output.1, reason)
            }
            Playback::Paused(state) => {
                let output = state
                    .buffer_control()
                    .output_state()
                    .unwrap_or((Millis::default(), previous_volume));
                (*state, output.0, output.1, None)
            }
            Playback::Draining { state, reason } => {
                let output = state
                    .buffer_control()
                    .output_state()
                    .unwrap_or((Millis::default(), previous_volume));
                (*state, output.0, output.1, Some(reason))
            }
            Playback::Idle(producer) => {
                self.playback = Playback::Idle(producer);
                return Ok(());
            }
            Playback::Empty => return Err(PlayerError::WorkerGone),
        };
        self.apply_processing(state, position, heard_volume, draining, update).await
    }

    async fn apply_processing<F>(
        &mut self,
        mut state: WorkerState,
        position: Millis,
        heard_volume: Volume,
        draining: Option<TrackEndReason>,
        update: F,
    ) -> Result<(), PlayerError>
    where
        F: FnOnce(&mut WorkerState, bool) -> Result<(), PlayerError>,
    {
        let seekable = state.is_seekable();
        if seekable {
            let (next, result) = tokio::task::spawn_blocking(move || {
                let result = state.seek(position);
                (state, result)
            })
            .await
            .map_err(|_| PlayerError::WorkerGone)?;
            state = next;
            if let Err(error) = result {
                return Err(self.fail_track(state, error).await);
            }
            state.producer_mut().clear_now_preserving_output();
            state.producer_mut().awaiting_frame();
            self.end_seen = self.buffer.end_sequence();
            state.transition_volume(heard_volume, self.volume);
        }
        let result = update(&mut state, seekable);
        state.producer_mut().set_paused(self.paused);
        state.producer_mut().set_end_time(self.end_time);
        if let Some(reason) = draining.filter(|_| !seekable) {
            self.playback = Playback::Draining { state: Box::new(state), reason };
        } else if self.paused {
            self.playback = Playback::Paused(Box::new(state));
        } else {
            self.launch(state);
        }
        result
    }
}

impl PlayerOwner {
    pub(crate) async fn seek(&mut self, position: Millis) -> Result<bool, PlayerError> {
        self.settle().await?;
        if self.track.is_none() {
            return Ok(false);
        }
        self.next = None;
        let playback = self.take_playback();
        let mut state = match playback {
            Playback::Paused(state) | Playback::Draining { state, .. } => *state,
            Playback::Running { signal, join } => {
                signal.pause();
                match join.await.map_err(|_| PlayerError::WorkerGone)? {
                    WorkerEnd::Paused(state) | WorkerEnd::Finished(state) => state,
                    end @ WorkerEnd::Failed { .. } => {
                        return match self.store_worker_end(end).await {
                            Err(error) => Err(error),
                            Ok(()) => Ok(false),
                        };
                    }
                    WorkerEnd::Stopped(state) => {
                        self.playback = Playback::Idle(state.into_producer());
                        self.track = None;
                        self.end_time = None;
                        return Ok(false);
                    }
                }
            }
            Playback::Idle(producer) => {
                producer.set_end_time(None);
                self.playback = Playback::Idle(producer);
                self.track = None;
                self.end_time = None;
                return Ok(false);
            }
            Playback::Empty => return Err(PlayerError::WorkerGone),
        };
        let (next, result) = tokio::task::spawn_blocking(move || {
            let result = state.seek(position);
            (state, result)
        })
        .await
        .map_err(|_| PlayerError::WorkerGone)?;
        state = next;
        let sought = match result {
            Ok(sought) => sought,
            Err(error) => {
                return Err(self.fail_track(state, error).await);
            }
        };
        if sought {
            self.buffer.markers().check_seek(position);
            self.end_seen = self.buffer.end_sequence();
        }
        state.producer_mut().set_end_time(self.end_time);
        if self.paused {
            state.producer_mut().set_paused(true);
            self.playback = Playback::Paused(Box::new(state));
        } else {
            self.launch(state);
        }
        Ok(sought)
    }

    pub(crate) fn replace_media(
        &mut self,
        playback: OpenedSource,
        position: Millis,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PlayerError>> + Send + '_>> {
        Box::pin(async move {
            self.settle().await?;
            if self.track.is_none() {
                return Ok(false);
            }
            let Some(state) = self.take_worker_state().await? else {
                return Ok(false);
            };
            let mut state = Box::new(state);
            let runtime = self.runtime.clone();
            let opened = tokio::task::spawn_blocking(move || {
                let mut decoder = BlockingDecoder::open(playback, runtime)?;
                if position != Millis::default() && !decoder.seek(position)? {
                    return Ok::<_, PlayerError>(None);
                }
                Ok(Some(decoder))
            })
            .await
            .map_err(|_| PlayerError::WorkerGone)?;
            let Some(decoder) = opened? else {
                self.restore_state(*state);
                return Ok(false);
            };
            state.replace_media(decoder, position);
            self.next = None;
            self.restore_state(*state);
            Ok(true)
        })
    }
}

impl PlayerOwner {
    pub(crate) async fn stuck_detected(&mut self) {
        let position = self.current_position();
        let Some(track) = self.track.as_ref().map(|track| track.snapshot(position)) else {
            return;
        };
        let stack_trace = Some(std::backtrace::Backtrace::force_capture().to_string());
        self.emit_event(PlayerEvent::TrackStuck {
            track,
            threshold_ms: self.buffer.stuck_threshold_ms(),
            stack_trace,
        })
        .await;
    }
}

#[cfg(not(test))]
const CHECK_INTERVAL: Duration = Duration::from_secs(10);
#[cfg(test)]
const CHECK_INTERVAL: Duration = Duration::from_millis(10);

impl PlayerOwner {
    pub(crate) fn arm_cleanup(&mut self) {
        let now = Instant::now();
        let _ = self.buffer.take_request();
        self.cleanup_last_request = now;
        self.cleanup_deadline = Some(now + CHECK_INTERVAL);
    }

    pub(crate) fn disarm_cleanup(&mut self) {
        self.cleanup_deadline = None;
    }

    pub(crate) fn cleanup_wait(&mut self) -> Option<Duration> {
        self.track.as_ref()?;
        let now = Instant::now();
        let deadline = *self.cleanup_deadline.get_or_insert(now + CHECK_INTERVAL);
        Some(deadline.saturating_duration_since(now))
    }

    pub(crate) async fn cleanup_tick(&mut self) -> Result<(), PlayerError> {
        let now = Instant::now();
        self.cleanup_deadline = Some(now + CHECK_INTERVAL);
        if self.buffer.take_request() {
            self.cleanup_last_request = now;
        }
        if self.track.is_some()
            && now.duration_since(self.cleanup_last_request)
                >= Duration::from_millis(self.config.cleanup_threshold_ms)
        {
            self.stop_with_reason(TrackEndReason::Cleanup).await?;
        }
        Ok(())
    }
}
