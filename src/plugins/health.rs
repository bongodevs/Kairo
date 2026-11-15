use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use player::ExceptionSeverity;
use player::source::SourceError;

/// Consecutive failures that close a source.
const FAILURE_LIMIT: u32 = 5;
/// How long a closed source stays down.
const OPEN_TIME: Duration = Duration::from_secs(10);
/// Returned instead of calling a closed source.
const BUSY_MESSAGE: &str = "This source is temporarily unavailable, try again in a few seconds.";

/// Consecutive call failures for one source name.
pub(super) struct SourceHealth {
    failures: AtomicU32,
    open_until: AtomicU64,
    started: Instant,
}

impl SourceHealth {
    pub(super) fn new() -> Self {
        Self { failures: AtomicU32::new(0), open_until: AtomicU64::new(0), started: Instant::now() }
    }

    /// Refuse the call with the busy message while the source is closed.
    pub(super) fn gate(&self) -> Result<(), SourceError> {
        if self.available() {
            Ok(())
        } else {
            Err(SourceError::load_failed(BUSY_MESSAGE, ExceptionSeverity::Common))
        }
    }

    pub(super) fn succeeded(&self) {
        self.failures.store(0, Ordering::Relaxed);
    }

    pub(super) fn failed(&self) {
        let failures = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= FAILURE_LIMIT {
            let open_until = self.elapsed_ms() + OPEN_TIME.as_millis() as u64;
            self.open_until.store(open_until, Ordering::Relaxed);
        }
    }

    fn available(&self) -> bool {
        self.open_until.load(Ordering::Relaxed) <= self.elapsed_ms()
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    #[cfg(test)]
    fn expire(&self) {
        self.open_until.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "../tests/plugins_health.rs"]
mod tests;
