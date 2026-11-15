//! Playback engine and track lifecycle.
//!
//! The core track model lives in `kairo_core::track`; this module re-exports it and adds the
//! lifecycle that drives one track through a player.

use tokio::task::JoinError;

pub use kairo_core::track::codec;
pub use kairo_core::track::*;

use crate::decode::{BlockingDecoder, Millis};
use crate::error::PlayerError;
use crate::event::{PlayerEvent, TrackEndReason};
use crate::opus::OpusFrameEncoder;
use crate::pipeline::{Playback, WorkerEnd, WorkerState};
use crate::player::PlayerOwner;

pub(crate) mod transition;

impl PlayerOwner {
    pub(crate) async fn play(
        &mut self,
        mut track: Track,
        no_replace: bool,
    ) -> Result<bool, PlayerError> {
        self.settle().await?;
        if no_replace && self.track.is_some() {
            return Ok(false);
        }
        let resolved = track.info().is_some();
        let Some(source) = self.sources.open_track(&track).await? else {
            return Err(PlayerError::NoMatches);
        };
        let runtime = self.runtime.clone();
        let initial_position = track.initial_position();
        let encoder_config = self.config.opus;
        let (decoder, encoder) = tokio::task::spawn_blocking(move || {
            let mut decoder = BlockingDecoder::open(source, runtime)?;
            if initial_position != Millis::default() {
                let _ = decoder.seek(initial_position)?;
            }
            let encoder = OpusFrameEncoder::new(encoder_config)?;
            Ok::<_, PlayerError>((decoder, encoder))
        })
        .await
        .map_err(|_| PlayerError::WorkerGone)??;
        if !resolved {
            track.resolve(
                TrackInfo::from_stream(decoder.info()),
                SourceData::probe(decoder.info().container, None),
            );
        }

        self.settle().await?;
        self.next = None;
        let end_time = self.end_time;
        self.stop_with_reason(TrackEndReason::Replaced).await?;
        self.end_time = end_time;
        let Playback::Idle(producer) = self.take_playback() else {
            return Err(PlayerError::WorkerGone);
        };
        let mut state = match WorkerState::new(
            decoder,
            encoder,
            producer,
            self.config,
            self.volume,
            self.filters,
        ) {
            Ok(state) => state,
            Err((error, producer)) => {
                self.playback = Playback::Idle(producer);
                return Err(error);
            }
        };
        state.producer_mut().begin_track();
        state.producer_mut().set_paused(self.paused);
        state.producer_mut().set_end_time(self.end_time);
        let snapshot = track.snapshot(initial_position);
        self.track = Some(track);
        self.arm_cleanup();
        self.emit_event(PlayerEvent::TrackStart { track: snapshot }).await;
        if self.paused {
            self.playback = Playback::Paused(Box::new(state));
        } else {
            self.launch(state);
        }
        Ok(true)
    }
}

impl PlayerOwner {
    pub(crate) async fn stop_with_reason(
        &mut self,
        reason: TrackEndReason,
    ) -> Result<(), PlayerError> {
        let position = self.current_position();
        self.next = None;
        let playback = self.take_playback();
        let mut state = match playback {
            Playback::Idle(producer) => {
                self.playback = Playback::Idle(producer);
                return self.emit_end(reason, position).await;
            }
            Playback::Paused(state) | Playback::Draining { state, .. } => *state,
            Playback::Running { signal, join } => {
                signal.stop();
                match join.await {
                    Ok(end) => self.state_for_stop(end).await,
                    Err(error) => return self.lost_worker(error, reason).await,
                }
            }
            Playback::Empty => return Err(PlayerError::WorkerGone),
        };
        state.clear_transition_state();
        state.producer_mut().clear_now();
        state.producer_mut().end_track();
        state.producer_mut().set_paused(self.paused);
        state.producer_mut().set_end_time(None);
        self.playback = Playback::Idle(state.into_producer());
        self.emit_end(reason, position).await
    }

    pub(crate) async fn worker_completed(
        &mut self,
        result: Result<WorkerEnd, JoinError>,
    ) -> Result<(), PlayerError> {
        let Playback::Running { .. } = self.take_playback() else {
            return Ok(());
        };
        match result {
            Ok(end) => self.store_worker_end(end).await,
            Err(error) => self.lost_worker(error, self.failure_reason()).await,
        }
    }

    pub(crate) async fn buffer_ended(&mut self) -> Result<(), PlayerError> {
        if self.track.is_none() {
            return Ok(());
        }
        let playback = self.take_playback();
        let (state, reason) = match playback {
            Playback::Draining { state, reason } => (*state, reason),
            Playback::Running { signal, join } => {
                signal.stop();
                match join.await {
                    Ok(end) => self.state_and_reason(end).await,
                    Err(error) => return self.lost_worker(error, self.failure_reason()).await,
                }
            }
            Playback::Paused(state) => (*state, TrackEndReason::Finished),
            Playback::Idle(producer) => {
                self.playback = Playback::Idle(producer);
                let position = self.current_position();
                return self.emit_end(TrackEndReason::Finished, position).await;
            }
            Playback::Empty => return Err(PlayerError::WorkerGone),
        };
        self.finish_track(state, reason).await
    }

    pub(crate) async fn settle(&mut self) -> Result<(), PlayerError> {
        let finished = matches!(
            &self.playback,
            Playback::Running { join, .. } if join.is_finished()
        );
        let mut worker_error = None;
        if finished {
            let Playback::Running { join, .. } = self.take_playback() else {
                return Ok(());
            };
            worker_error = match join.await {
                Ok(end) => self.store_worker_end(end).await.err(),
                Err(error) => self.lost_worker(error, self.failure_reason()).await.err(),
            };
        }
        let sequence = self.buffer.end_sequence();
        if sequence != self.end_seen {
            self.end_seen = sequence;
            self.buffer_ended().await?;
        }
        let stuck_sequence = self.buffer.stuck_sequence();
        if stuck_sequence != self.stuck_seen {
            self.stuck_seen = stuck_sequence;
            self.stuck_detected().await;
        }
        match worker_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(crate) async fn store_worker_end(&mut self, end: WorkerEnd) -> Result<(), PlayerError> {
        match end {
            WorkerEnd::Finished(state) => {
                self.playback =
                    Playback::Draining { state: Box::new(state), reason: TrackEndReason::Finished };
                Ok(())
            }
            WorkerEnd::Failed { state, error, end_queued } => {
                let reason = failure_reason(&state);
                self.emit_exception(&error).await;
                if end_queued {
                    self.playback = Playback::Draining { state: Box::new(state), reason };
                } else {
                    let _ = self.finish_track(state, reason).await;
                }
                Err(error)
            }
            WorkerEnd::Paused(state) => {
                self.playback = Playback::Paused(Box::new(state));
                Ok(())
            }
            WorkerEnd::Stopped(state) => self.finish_track(state, TrackEndReason::Stopped).await,
        }
    }

    pub(crate) async fn fail_track(
        &mut self,
        state: WorkerState,
        error: PlayerError,
    ) -> PlayerError {
        let reason = failure_reason(&state);
        self.emit_exception(&error).await;
        let _ = self.finish_track(state, reason).await;
        error
    }

    async fn state_for_stop(&mut self, end: WorkerEnd) -> WorkerState {
        match end {
            WorkerEnd::Paused(state) | WorkerEnd::Stopped(state) | WorkerEnd::Finished(state) => {
                state
            }
            WorkerEnd::Failed { state, error, .. } => {
                self.emit_exception(&error).await;
                state
            }
        }
    }

    async fn state_and_reason(&mut self, end: WorkerEnd) -> (WorkerState, TrackEndReason) {
        match end {
            WorkerEnd::Failed { state, error, .. } => {
                let reason = failure_reason(&state);
                self.emit_exception(&error).await;
                (state, reason)
            }
            WorkerEnd::Paused(state) | WorkerEnd::Stopped(state) | WorkerEnd::Finished(state) => {
                (state, TrackEndReason::Finished)
            }
        }
    }

    async fn finish_track(
        &mut self,
        mut state: WorkerState,
        reason: TrackEndReason,
    ) -> Result<(), PlayerError> {
        let position = self.current_position();
        self.next = None;
        state.clear_transition_state();
        state.producer_mut().clear_now();
        state.producer_mut().end_track();
        state.producer_mut().set_paused(self.paused);
        state.producer_mut().set_end_time(None);
        self.playback = Playback::Idle(state.into_producer());
        self.emit_end(reason, position).await
    }

    async fn lost_worker(
        &mut self,
        _source: JoinError,
        reason: TrackEndReason,
    ) -> Result<(), PlayerError> {
        let error = PlayerError::WorkerGone;
        self.emit_exception(&error).await;
        self.buffer.end_track();
        self.playback = Playback::Empty;
        let position = self.current_position();
        self.emit_end(reason, position).await?;
        Err(error)
    }

    fn failure_reason(&self) -> TrackEndReason {
        if self.buffer.has_received_frames() {
            TrackEndReason::Finished
        } else {
            TrackEndReason::LoadFailed
        }
    }
}

fn failure_reason(state: &WorkerState) -> TrackEndReason {
    if state.has_received_frames() { TrackEndReason::Finished } else { TrackEndReason::LoadFailed }
}
