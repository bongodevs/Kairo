use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use tokio::runtime::Handle;
use tokio::sync::Notify;

use crate::decode::Millis;
use crate::error::PlayerError;
use crate::pipeline::{EndSend, FrameControl, WorkerState};
use crate::track::transition::{Promotion, PromotionSlot};

pub(crate) enum WorkerEnd {
    Paused(WorkerState),
    Stopped(WorkerState),
    Finished(WorkerState),
    Failed { state: WorkerState, error: PlayerError, end_queued: bool },
}

pub(crate) fn run_worker(
    mut state: WorkerState,
    signal: Arc<WorkerSignal>,
    runtime: Handle,
) -> WorkerEnd {
    let mut failure = None;
    loop {
        match signal.action() {
            WorkerAction::Pause => {
                publish_promotions(&mut state, &signal);
                return match failure {
                    Some(error) => WorkerEnd::Failed { state, error, end_queued: false },
                    None => WorkerEnd::Paused(state),
                };
            }
            WorkerAction::Stop => {
                publish_promotions(&mut state, &signal);
                return match failure {
                    Some(error) => WorkerEnd::Failed { state, error, end_queued: false },
                    None => WorkerEnd::Stopped(state),
                };
            }
            WorkerAction::Run => {}
        }

        if let Some(frame) = state.pending.pop_front() {
            if signal.reached_end(frame.meta().position) {
                state.pending.clear();
                if state.has_successor() {
                    let reason = state.transition_reason();
                    let produced = cpu::run(&runtime, || state.finish_transition(reason));
                    publish_promotions(&mut state, &signal);
                    if let Err(error) = produced {
                        failure = Some(error);
                        state.ending = true;
                    }
                } else {
                    state.ending = true;
                }
                continue;
            }
            match runtime.block_on(state.producer.send_or_wake(frame, signal.wake())) {
                Ok(outcome) => {
                    if let Some(frame) = outcome.into_woken() {
                        state.pending.push_front(frame);
                    }
                    continue;
                }
                Err(error) => {
                    return WorkerEnd::Failed { state, error: error.into(), end_queued: false };
                }
            }
        }
        if state.ending {
            publish_promotions(&mut state, &signal);
            match runtime.block_on(state.producer.send_end_or_wake(signal.wake())) {
                Ok(EndSend::Sent) => {
                    return match failure {
                        Some(error) => WorkerEnd::Failed { state, error, end_queued: true },
                        None => WorkerEnd::Finished(state),
                    };
                }
                Ok(EndSend::Woken) => continue,
                Err(error) => {
                    return WorkerEnd::Failed {
                        state,
                        error: failure.unwrap_or_else(|| error.into()),
                        end_queued: false,
                    };
                }
            }
        }

        state.producer.note_worker_progress();
        let packet = state.read_packet();
        let produced = match packet {
            Ok(packet) => cpu::run(&runtime, || state.process_packet(packet)),
            Err(error) => Err(error),
        };
        publish_promotions(&mut state, &signal);
        state.producer.note_worker_progress();
        if let Err(error) = produced {
            state.pending.clear();
            state.ending = true;
            failure = Some(error);
        }
    }
}

fn publish_promotions(state: &mut WorkerState, signal: &WorkerSignal) {
    while let Some(promotion) = state.take_promotion() {
        signal.publish_promotion(promotion);
    }
}

pub(crate) mod cpu {
    use std::sync::OnceLock;

    use tokio::runtime::Handle;
    use tokio::sync::Semaphore;

    fn slots() -> &'static Semaphore {
        static SLOTS: OnceLock<Semaphore> = OnceLock::new();
        SLOTS.get_or_init(|| Semaphore::new(slot_count()))
    }

    pub(crate) fn run<R>(runtime: &Handle, work: impl FnOnce() -> R) -> R {
        let Ok(permit) = runtime.block_on(slots().acquire()) else {
            return work();
        };
        let result = work();
        drop(permit);
        result
    }

    pub(crate) fn slot_count() -> usize {
        std::thread::available_parallelism().map_or(1, usize::from)
    }
}

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const STOP: u8 = 2;
const NO_END_TIME: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerAction {
    Run,
    Pause,
    Stop,
}

pub(crate) struct WorkerSignal {
    command: AtomicU8,
    wake: Notify,
    end_time: AtomicU64,
    buffer: FrameControl,
    promotion: Arc<PromotionSlot>,
}

impl WorkerSignal {
    pub(crate) fn new(
        end_time: Option<Millis>,
        buffer: FrameControl,
        promotion: Arc<PromotionSlot>,
    ) -> Arc<Self> {
        buffer.set_end_time(end_time);
        Arc::new(Self {
            command: AtomicU8::new(RUN),
            wake: Notify::new(),
            end_time: AtomicU64::new(end_time.map_or(NO_END_TIME, Millis::get)),
            buffer,
            promotion,
        })
    }

    pub(crate) fn pause(&self) {
        self.command.store(PAUSE, Ordering::Release);
        self.wake.notify_one();
    }

    pub(crate) fn stop(&self) {
        self.command.store(STOP, Ordering::Release);
        self.wake.notify_one();
    }

    pub(crate) fn set_end_time(&self, end_time: Option<Millis>) {
        self.end_time.store(end_time.map_or(NO_END_TIME, Millis::get), Ordering::Release);
        self.buffer.set_end_time(end_time);
        self.wake.notify_one();
    }

    pub(crate) fn set_buffer_paused(&self, paused: bool) {
        self.buffer.set_paused(paused);
    }

    pub(crate) fn output_state(&self) -> Option<(Millis, crate::volume::Volume)> {
        self.buffer.output_state()
    }

    pub(super) fn reached_end(&self, position: Millis) -> bool {
        let end_time = self.end_time.load(Ordering::Acquire);
        end_time != NO_END_TIME && position.get() >= end_time
    }

    pub(super) fn publish_promotion(&self, promotion: Promotion) {
        self.promotion.publish(promotion);
    }

    pub(super) fn action(&self) -> WorkerAction {
        match self.command.load(Ordering::Acquire) {
            PAUSE => WorkerAction::Pause,
            STOP => WorkerAction::Stop,
            _ => WorkerAction::Run,
        }
    }

    pub(super) fn wake(&self) -> &Notify {
        &self.wake
    }
}
