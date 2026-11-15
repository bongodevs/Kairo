use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::Notify;
use tokio::sync::mpsc;

use super::consumer::FrameConsumer;
use super::{
    BufferError, BufferedFrame, EndSend, FrameBufferConfig, FrameControl, FrameSend, NO_POSITION,
    Queued, State, decrement,
};

pub fn frame_buffer(config: FrameBufferConfig) -> (FrameProducer, FrameConsumer) {
    let (sender, receiver) = mpsc::channel(config.capacity());
    let state = Arc::new(State::default());
    (
        FrameProducer { sender, state: state.clone(), generation: 0, clear_on_insert: false },
        FrameConsumer::new(receiver, state),
    )
}

pub struct FrameProducer {
    sender: mpsc::Sender<Queued>,
    state: Arc<State>,
    generation: u64,
    clear_on_insert: bool,
}

impl FrameProducer {
    pub async fn send(&mut self, frame: BufferedFrame) -> Result<(), BufferError> {
        let (queued, activates, position) = self.queued(frame);
        let permit = self.sender.reserve().await.map_err(|_| BufferError::Closed)?;
        self.state.depth.fetch_add(1, Ordering::Relaxed);
        permit.send(queued);
        self.sent(activates, position);
        Ok(())
    }

    /// Wait for capacity or return the still-owned frame when control wakes the worker.
    pub async fn send_or_wake(
        &mut self,
        frame: BufferedFrame,
        wake: &Notify,
    ) -> Result<FrameSend, BufferError> {
        let (queued, activates, position) = self.queued(frame);
        let permit = tokio::select! {
            permit = self.sender.reserve() => permit.map_err(|_| BufferError::Closed)?,
            () = wake.notified() => {
                let Some(frame) = queued.frame else {
                    return Err(BufferError::Closed);
                };
                return Ok(FrameSend::woken(frame));
            }
        };
        self.state.depth.fetch_add(1, Ordering::Relaxed);
        permit.send(queued);
        self.sent(activates, position);
        Ok(FrameSend::sent())
    }

    /// Backpressure for a dedicated blocking worker. Never call from an async runtime worker.
    pub fn blocking_send(&mut self, frame: BufferedFrame) -> Result<(), BufferError> {
        let (queued, activates, position) = self.queued(frame);
        self.state.depth.fetch_add(1, Ordering::Relaxed);
        if self.sender.blocking_send(queued).is_err() {
            decrement(&self.state.depth);
            return Err(BufferError::Closed);
        }
        self.sent(activates, position);
        Ok(())
    }

    pub async fn send_end(&mut self) -> Result<(), BufferError> {
        let (queued, activates) = self.queued_end();
        let permit = self.sender.reserve().await.map_err(|_| BufferError::Closed)?;
        self.state.depth.fetch_add(1, Ordering::Relaxed);
        permit.send(queued);
        self.activate(activates);
        Ok(())
    }

    pub async fn send_end_or_wake(&mut self, wake: &Notify) -> Result<EndSend, BufferError> {
        let (queued, activates) = self.queued_end();
        let permit = tokio::select! {
            permit = self.sender.reserve() => permit.map_err(|_| BufferError::Closed)?,
            () = wake.notified() => return Ok(EndSend::Woken),
        };
        self.state.depth.fetch_add(1, Ordering::Relaxed);
        permit.send(queued);
        self.activate(activates);
        Ok(EndSend::Sent)
    }

    pub async fn finish(mut self) -> Result<(), BufferError> {
        self.send_end().await
    }

    pub fn clear_on_insert(&mut self) {
        self.clear_on_insert = true;
        self.awaiting_frame();
    }

    pub fn clear_now(&mut self) {
        self.clear_now_with_output(true);
    }

    pub(crate) fn clear_now_preserving_output(&mut self) {
        self.clear_now_with_output(false);
    }

    fn clear_now_with_output(&mut self, reset_output: bool) {
        self.generation =
            self.generation.max(self.state.active.load(Ordering::Acquire)).wrapping_add(1);
        self.clear_on_insert = false;
        self.state.discontinuity.store(false, Ordering::Release);
        self.state.stuck_reset.fetch_add(1, Ordering::AcqRel);
        self.state.active.fetch_max(self.generation, Ordering::AcqRel);
        self.state.last_position.store(NO_POSITION, Ordering::Relaxed);
        if reset_output {
            self.state.output_position.store(NO_POSITION, Ordering::Relaxed);
            self.state.next_output_position.store(NO_POSITION, Ordering::Release);
        }
    }

    pub(crate) fn begin_track(&mut self) {
        self.clear_now();
        self.state.received.store(false, Ordering::Release);
        self.state.playing.store(true, Ordering::Release);
        self.state.track_epoch.fetch_add(1, Ordering::AcqRel);
        self.state.note_request();
        self.awaiting_frame();
    }

    /// Mark a hand-off to a new track without clearing the frames already queued for the mixed
    /// overlap. Consumers reset their per-track state (tape, stuck detection) on this epoch.
    pub(crate) fn track_boundary(&self) {
        self.state.track_epoch.fetch_add(1, Ordering::AcqRel);
        self.state.stuck_reset.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn end_track(&self) {
        self.state.playing.store(false, Ordering::Release);
        self.state.discontinuity.store(false, Ordering::Release);
        self.state.stuck_reset.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn awaiting_frame(&self) {
        self.state.discontinuity.store(true, Ordering::Release);
        self.state.stuck_reset.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn note_worker_progress(&self) {
        self.state.worker_progress.fetch_add(1, Ordering::AcqRel);
    }

    pub fn set_paused(&self, paused: bool) {
        self.state.paused.store(paused, Ordering::Release);
        self.state.stuck_reset.fetch_add(1, Ordering::AcqRel);
    }

    pub fn set_end_time(&self, end_time: Option<crate::decode::Millis>) {
        set_end_time(&self.state, end_time);
    }

    pub(crate) fn control(&self) -> FrameControl {
        FrameControl::new(self.state.clone())
    }

    #[must_use]
    pub fn remaining_capacity(&self) -> usize {
        self.sender.capacity()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.sender.max_capacity()
    }

    #[must_use]
    pub fn has_received_frames(&self) -> bool {
        self.state.received.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn last_input_position(&self) -> Option<crate::decode::Millis> {
        let position = self.state.last_position.load(Ordering::Relaxed);
        (position != NO_POSITION).then_some(crate::decode::Millis::new(position))
    }

    fn queued(&self, frame: BufferedFrame) -> (Queued, Option<u64>, u64) {
        let (generation, activates) = self.next_generation();
        let position = frame.meta().position.get();
        (Queued { generation, frame: Some(frame) }, activates, position)
    }

    fn queued_end(&self) -> (Queued, Option<u64>) {
        let (generation, activates) = self.next_generation();
        (Queued { generation, frame: None }, activates)
    }

    fn next_generation(&self) -> (u64, Option<u64>) {
        let active = self.state.active.load(Ordering::Acquire);
        let generation = if self.clear_on_insert {
            active.max(self.generation).wrapping_add(1)
        } else {
            active.max(self.generation)
        };
        (generation, self.clear_on_insert.then_some(generation))
    }

    fn sent(&mut self, activates: Option<u64>, position: u64) {
        self.state.received.store(true, Ordering::Relaxed);
        self.activate(activates);
        self.state.last_position.store(position, Ordering::Relaxed);
    }

    fn activate(&mut self, activates: Option<u64>) {
        self.generation = self.state.active.load(Ordering::Acquire).max(self.generation);
        if let Some(generation) = activates {
            let active = self.state.active.fetch_max(generation, Ordering::AcqRel);
            self.generation = self.generation.max(generation).max(active);
            self.clear_on_insert = false;
        }
        self.state.discontinuity.store(false, Ordering::Release);
    }
}

fn set_end_time(state: &State, end_time: Option<crate::decode::Millis>) {
    state
        .end_time
        .store(end_time.map_or(NO_POSITION, crate::decode::Millis::get), Ordering::Release);
}
