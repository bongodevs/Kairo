use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Per-provider counters and latency, shared by every clone of a provider.
#[derive(Debug, Default)]
pub struct Metrics {
    loads: AtomicU64,
    load_errors: AtomicU64,
    load_micros: AtomicU64,
    opens: AtomicU64,
    open_errors: AtomicU64,
    open_micros: AtomicU64,
}

/// The values behind [`Metrics`], safe to read from any thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub loads: u64,
    pub load_errors: u64,
    pub load_micros: u64,
    pub opens: u64,
    pub open_errors: u64,
    pub open_micros: u64,
}

impl Metrics {
    pub(crate) fn record_load(&self, elapsed: Duration, ok: bool) {
        self.loads.fetch_add(1, Ordering::Relaxed);
        self.load_micros.fetch_add(micros(elapsed), Ordering::Relaxed);
        if !ok {
            self.load_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn record_open(&self, elapsed: Duration, ok: bool) {
        self.opens.fetch_add(1, Ordering::Relaxed);
        self.open_micros.fetch_add(micros(elapsed), Ordering::Relaxed);
        if !ok {
            self.open_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            loads: self.loads.load(Ordering::Relaxed),
            load_errors: self.load_errors.load(Ordering::Relaxed),
            load_micros: self.load_micros.load(Ordering::Relaxed),
            opens: self.opens.load(Ordering::Relaxed),
            open_errors: self.open_errors.load(Ordering::Relaxed),
            open_micros: self.open_micros.load(Ordering::Relaxed),
        }
    }
}

impl MetricsSnapshot {
    /// Mean load latency, or `None` before the first load.
    #[must_use]
    pub const fn average_load_micros(&self) -> Option<u64> {
        self.load_micros.checked_div(self.loads)
    }

    /// Mean open latency, or `None` before the first open.
    #[must_use]
    pub const fn average_open_micros(&self) -> Option<u64> {
        self.open_micros.checked_div(self.opens)
    }
}

fn micros(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}
