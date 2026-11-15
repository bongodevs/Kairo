use std::backtrace::Backtrace;
use std::error::Error;

use tokio::sync::mpsc;

use kairo_core::track::TrackSnapshot;

use crate::ExceptionSeverity;
use crate::decode::Millis;
use crate::error::PlayerError;
use crate::player::PlayerOwner;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackEndReason {
    Finished,
    LoadFailed,
    Stopped,
    Replaced,
    Cleanup,
    Crossfade,
    Gapless,
}

impl TrackEndReason {
    #[must_use]
    pub const fn may_start_next(self) -> bool {
        matches!(self, Self::Finished | Self::LoadFailed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackException {
    pub message: Option<String>,
    pub severity: ExceptionSeverity,
    pub cause: String,
    pub cause_stack_trace: String,
}

impl TrackException {
    pub(crate) fn from_error(error: &PlayerError) -> Self {
        let mut cause: &(dyn Error + 'static) = error;
        while let Some(source) = cause.source() {
            cause = source;
        }
        Self {
            message: Some(error.to_string()),
            severity: error.severity(),
            cause: cause.to_string(),
            cause_stack_trace: Backtrace::force_capture().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    TrackStart { track: TrackSnapshot },
    TrackEnd { track: TrackSnapshot, reason: TrackEndReason },
    TrackException { track: TrackSnapshot, exception: TrackException },
    TrackStuck { track: TrackSnapshot, threshold_ms: u64 },
    Paused { track: TrackSnapshot },
    Resumed { track: TrackSnapshot },
}

pub struct PlayerEvents {
    pub(crate) receiver: mpsc::Receiver<PlayerEvent>,
}

impl PlayerEvents {
    pub async fn recv(&mut self) -> Option<PlayerEvent> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<PlayerEvent, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn close(&mut self) {
        self.receiver.close();
    }
}

/// Event delivery for one player. A stalled observer loses new events; it never stalls the
/// playback owner.
impl PlayerOwner {
    pub(crate) async fn emit_event(&mut self, event: PlayerEvent) {
        let _ = self.events.try_send(event);
    }

    pub(crate) async fn emit_end(
        &mut self,
        reason: TrackEndReason,
        position: Millis,
    ) -> Result<(), PlayerError> {
        self.end_time = None;
        self.disarm_cleanup();
        self.buffer.end_track();
        if let Some(track) = self.track.take() {
            self.emit_event(PlayerEvent::TrackEnd { track: track.snapshot(position), reason })
                .await;
        }
        Ok(())
    }

    pub(crate) async fn emit_exception(&mut self, error: &PlayerError) {
        let position = self.current_position();
        let Some(track) = self.track.as_ref().map(|track| track.snapshot(position)) else {
            return;
        };
        self.emit_event(PlayerEvent::TrackException {
            track,
            exception: TrackException::from_error(error),
        })
        .await;
    }
}
