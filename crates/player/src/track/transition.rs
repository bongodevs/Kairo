use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Notify;

use kairo_core::track::{SourceData, Track, TrackInfo};

use crate::decode::{BlockingDecoder, Millis};
use crate::effects::CrossfadeOptions;
use crate::error::PlayerError;
use crate::event::{PlayerEvent, TrackEndReason};
use crate::pipeline::{Playback, WorkerEnd, WorkerState};
use crate::player::PlayerOwner;

/// A successor that has become the active track in the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Promotion {
    pub position: Millis,
    pub reason: TrackEndReason,
}

/// The worker's hand-off to the owner. One slot is enough: a worker promotes at most one
/// successor before the owner drains the notice, and the sequence lets the owner wait without
/// polling.
pub(crate) struct PromotionSlot {
    notice: Mutex<Option<Promotion>>,
    sequence: AtomicU64,
    notify: Notify,
}

impl PromotionSlot {
    pub(crate) fn new() -> Self {
        Self { notice: Mutex::new(None), sequence: AtomicU64::new(0), notify: Notify::new() }
    }

    pub(crate) fn publish(&self, notice: Promotion) {
        if let Ok(mut slot) = self.notice.lock() {
            *slot = Some(notice);
        }
        self.sequence.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_one();
    }

    pub(crate) fn take(&self) -> Option<Promotion> {
        self.notice.lock().ok().and_then(|mut slot| slot.take())
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    pub(crate) async fn wait_for(&self, seen: u64) -> u64 {
        loop {
            let notified = self.notify.notified();
            let current = self.sequence();
            if current != seen {
                return current;
            }
            notified.await;
        }
    }
}

impl PlayerOwner {
    pub(crate) fn prepare_next(
        &mut self,
        track: Track,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PlayerError>> + Send + '_>> {
        Box::pin(async move { self.prepare_next_impl(track).await })
    }

    async fn prepare_next_impl(&mut self, mut track: Track) -> Result<bool, PlayerError> {
        self.settle().await?;
        if self.track.is_none() {
            return Ok(false);
        }
        let Some(options) = self.crossfade else {
            return Ok(false);
        };
        if !matches!(self.playback, Playback::Running { .. } | Playback::Paused(_)) {
            return Ok(false);
        }

        let resolved = track.info().is_some();
        let Some(source) = self.sources.open_track(&track).await? else {
            return Err(PlayerError::NoMatches);
        };
        let runtime = self.runtime.clone();
        let initial_position = track.initial_position();
        let decoder = tokio::task::spawn_blocking(move || {
            let mut decoder = BlockingDecoder::open(source, runtime)?;
            if initial_position != Millis::default() {
                let _ = decoder.seek(initial_position)?;
            }
            Ok::<_, crate::decode::DecodeError>(decoder)
        })
        .await
        .map_err(|_| PlayerError::WorkerGone)??;
        if !resolved {
            track.resolve(
                TrackInfo::from_stream(decoder.info()),
                SourceData::probe(decoder.info().container, None),
            );
        }

        let Some(mut state) = self.take_worker_state().await? else {
            return Ok(false);
        };
        let Some((fade_start, options)) =
            schedule(options, state.base_length(), self.end_time, self.current_position())
        else {
            self.restore_state(state);
            return Ok(false);
        };
        state.prepare_next(decoder, initial_position, options, fade_start);
        self.next = Some(track);
        self.restore_state(state);
        Ok(true)
    }

    pub(crate) fn clear_next(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), PlayerError>> + Send + '_>> {
        Box::pin(async move { self.clear_next_impl().await })
    }

    async fn clear_next_impl(&mut self) -> Result<(), PlayerError> {
        self.settle().await?;
        if self.next.is_none() {
            return Ok(());
        }
        self.next = None;
        let Some(mut state) = self.take_worker_state().await? else {
            return Ok(());
        };
        state.clear_next();
        self.restore_state(state);
        Ok(())
    }

    pub(crate) fn set_crossfade(
        &mut self,
        options: Option<CrossfadeOptions>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PlayerError>> + Send + '_>> {
        Box::pin(async move { self.set_crossfade_impl(options).await })
    }

    async fn set_crossfade_impl(
        &mut self,
        options: Option<CrossfadeOptions>,
    ) -> Result<(), PlayerError> {
        self.settle().await?;
        if self.crossfade == options {
            return Ok(());
        }
        self.crossfade = options;
        self.rearm_crossfade().await
    }

    pub(crate) fn reset_crossfade(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), PlayerError>> + Send + '_>> {
        Box::pin(async move {
            self.settle().await?;
            let options = self.config.crossfade;
            if self.crossfade == options {
                return Ok(());
            }
            self.crossfade = options;
            self.rearm_crossfade().await
        })
    }

    async fn rearm_crossfade(&mut self) -> Result<(), PlayerError> {
        if self.next.is_none() {
            return Ok(());
        }
        let Some(mut state) = self.take_worker_state().await? else {
            return Ok(());
        };
        let options = self.crossfade;
        let schedule = options.and_then(|options| {
            schedule(options, state.base_length(), self.end_time, self.current_position())
        });
        match schedule {
            Some((fade_start, options)) => state.rearm_next(options, fade_start),
            None => {
                state.clear_next();
                self.next = None;
            }
        }
        self.restore_state(state);
        Ok(())
    }

    pub(crate) fn transition_now(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PlayerError>> + Send + '_>> {
        Box::pin(async move { self.transition_now_impl().await })
    }

    async fn transition_now_impl(&mut self) -> Result<bool, PlayerError> {
        self.settle().await?;
        if self.next.is_none() {
            return Ok(false);
        }
        let Some(options) = self.crossfade else {
            return Ok(false);
        };
        let Some(mut state) = self.take_worker_state().await? else {
            return Ok(false);
        };
        let started = state.transition_now(options);
        self.restore_state(state);
        Ok(started)
    }

    pub(crate) async fn process_promotion(&mut self) {
        let Some(notice) = self.promotion.take() else {
            return;
        };
        let Some(next) = self.next.take() else {
            return;
        };
        self.end_time = None;
        self.buffer.set_end_time(None);
        if let Playback::Running { signal, .. } = &self.playback {
            signal.set_end_time(None);
        }
        let position = self.current_position();
        if let Some(track) = self.track.take() {
            self.emit_event(PlayerEvent::TrackEnd {
                track: track.snapshot(position),
                reason: notice.reason,
            })
            .await;
        }
        self.track = Some(next);
        self.arm_cleanup();
        if let Some(track) = self.track.as_ref() {
            self.emit_event(PlayerEvent::TrackStart { track: track.snapshot(notice.position) })
                .await;
        }
    }

    async fn take_worker_state(&mut self) -> Result<Option<WorkerState>, PlayerError> {
        let playback = self.take_playback();
        match playback {
            Playback::Paused(state) => Ok(Some(*state)),
            Playback::Running { signal, join } => {
                signal.pause();
                match join.await.map_err(|_| PlayerError::WorkerGone)? {
                    WorkerEnd::Paused(state) => Ok(Some(state)),
                    WorkerEnd::Finished(state) => {
                        self.playback = Playback::Draining {
                            state: Box::new(state),
                            reason: TrackEndReason::Finished,
                        };
                        Ok(None)
                    }
                    end @ WorkerEnd::Failed { .. } => {
                        self.store_worker_end(end).await?;
                        Ok(None)
                    }
                    WorkerEnd::Stopped(state) => {
                        self.playback = Playback::Idle(state.into_producer());
                        self.track = None;
                        self.end_time = None;
                        Ok(None)
                    }
                }
            }
            Playback::Draining { state, reason } => {
                self.playback = Playback::Draining { state, reason };
                Ok(None)
            }
            Playback::Idle(producer) => {
                self.playback = Playback::Idle(producer);
                Ok(None)
            }
            Playback::Empty => Err(PlayerError::WorkerGone),
        }
    }

    fn restore_state(&mut self, mut state: WorkerState) {
        state.producer_mut().set_paused(self.paused);
        state.producer_mut().set_end_time(self.end_time);
        if self.paused {
            self.playback = Playback::Paused(Box::new(state));
        } else {
            self.launch(state);
        }
    }
}

fn schedule(
    options: CrossfadeOptions,
    length: Option<Millis>,
    end_time: Option<Millis>,
    position: Millis,
) -> Option<(Option<Millis>, CrossfadeOptions)> {
    if options.duration_ms == 0 {
        return options.gapless.then_some((None, options));
    }
    let end = match (length, end_time) {
        (Some(length), Some(end_time)) => Some(length.min(end_time)),
        (Some(length), None) => Some(length),
        (None, Some(end_time)) => Some(end_time),
        (None, None) => None,
    };
    let Some(end) = end else {
        return options.gapless.then_some((None, gapless(options)));
    };
    if end <= position || end.get().saturating_sub(position.get()) <= options.duration_ms {
        return options.gapless.then_some((None, gapless(options)));
    }
    Some((Some(Millis::new(end.get() - options.duration_ms)), options))
}

fn gapless(mut options: CrossfadeOptions) -> CrossfadeOptions {
    options.duration_ms = 0;
    options
}
