use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use player::ExceptionSeverity;
use player::source::SourceError;

/// Consecutive failures that close a source.
const FAILURE_LIMIT: u32 = 5;
/// How long a closed source stays down.
const OPEN_TIME: Duration = Duration::from_secs(10);
/// Returned instead of calling a closed source.
pub(super) const BUSY_MESSAGE: &str =
    "This source is temporarily unavailable, try again in a few seconds.";

const UP: u8 = 0;
const DOWN: u8 = 1;
const PROBING: u8 = 2;

/// The up/down breaker for one source name.
pub(super) struct SourceHealth {
    state: AtomicU8,
    failures: AtomicU32,
    open_until: AtomicU64,
    started: Instant,
}

impl SourceHealth {
    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(UP),
            failures: AtomicU32::new(0),
            open_until: AtomicU64::new(0),
            started: Instant::now(),
        }
    }

    /// Admit one call, claiming the single probe when the down window has passed.
    pub(super) fn enter(&self) -> Result<CallPermit<'_>, SourceError> {
        match self.state.load(Ordering::SeqCst) {
            UP => return Ok(self.permit(false)),
            PROBING => return Err(busy()),
            DOWN if self.elapsed_ms() < self.open_until.load(Ordering::SeqCst) => {
                return Err(busy());
            }
            _ => {}
        }
        match self.state.compare_exchange(DOWN, PROBING, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => Ok(self.permit(true)),
            Err(_) => Err(busy()),
        }
    }

    fn permit(&self, probe: bool) -> CallPermit<'_> {
        CallPermit { health: self, probe, finished: false }
    }

    fn succeeded(&self) {
        self.failures.store(0, Ordering::SeqCst);
        self.state.store(UP, Ordering::SeqCst);
    }

    fn failed(&self, probe: bool) {
        if probe || self.failures.fetch_add(1, Ordering::SeqCst) + 1 >= FAILURE_LIMIT {
            self.close();
        }
    }

    fn close(&self) {
        self.failures.store(0, Ordering::SeqCst);
        let open_until = self.elapsed_ms() + OPEN_TIME.as_millis() as u64;
        self.open_until.store(open_until, Ordering::SeqCst);
        self.state.store(DOWN, Ordering::SeqCst);
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    #[cfg(test)]
    fn expire(&self) {
        self.open_until.store(0, Ordering::SeqCst);
    }
}

/// One admitted call; dropping it before reporting counts as a failure.
pub(super) struct CallPermit<'a> {
    health: &'a SourceHealth,
    probe: bool,
    finished: bool,
}

impl CallPermit<'_> {
    pub(super) fn succeeded(mut self) {
        self.health.succeeded();
        self.finished = true;
    }

    pub(super) fn failed(mut self) {
        self.health.failed(self.probe);
        self.finished = true;
    }
}

impl Drop for CallPermit<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.health.failed(self.probe);
        }
    }
}

/// Whether this error is the gate's busy signal rather than a plugin failure.
pub(super) fn is_busy(error: &SourceError) -> bool {
    matches!(error, SourceError::LoadFailed { message, .. } if message == BUSY_MESSAGE)
}

fn busy() -> SourceError {
    SourceError::load_failed(BUSY_MESSAGE, ExceptionSeverity::Common)
}

#[cfg(test)]
#[path = "../tests/plugins_health.rs"]
mod tests;
