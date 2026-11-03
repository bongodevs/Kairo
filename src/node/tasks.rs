//! The registry of named background tasks, so a task can be replaced or cancelled by key.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::MissedTickBehavior;

use crate::utils::locked;

/// The process-wide task registry.
pub static TASKS: LazyLock<Tasks> = LazyLock::new(Tasks::default);

/// The key of a session's stats ticker.
pub fn session_stats(session_id: &str) -> String {
    format!("stats:{session_id}")
}

/// Interval tasks held by key, each cancelled when it is removed or replaced.
#[derive(Default)]
pub struct Tasks {
    runners: Mutex<BTreeMap<String, JoinHandle<()>>>,
}

impl Tasks {
    /// Run `handler` every `period` (a slow run pushes the next tick out), replacing any task
    /// registered under `key`.
    pub fn add<F, Fut>(&self, key: impl Into<String>, period: Duration, handler: F) -> AbortHandle
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.spawn(key.into(), period, MissedTickBehavior::Delay, handler)
    }

    /// Run `handler` on a fixed-rate grid: missed ticks fire immediately instead of pushing the
    /// schedule out.
    pub fn add_fixed_rate<F, Fut>(
        &self,
        key: impl Into<String>,
        period: Duration,
        handler: F,
    ) -> AbortHandle
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.spawn(key.into(), period, MissedTickBehavior::Burst, handler)
    }

    fn spawn<F, Fut>(
        &self,
        key: String,
        period: Duration,
        behavior: MissedTickBehavior,
        mut handler: F,
    ) -> AbortHandle
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let runner = tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            interval.set_missed_tick_behavior(behavior);
            loop {
                interval.tick().await;
                handler().await;
            }
        });
        let handle = runner.abort_handle();
        tracing::debug!(task = %key, secs = period.as_secs(), "started interval");
        if let Some(previous) = locked(&self.runners).insert(key, runner) {
            previous.abort();
        }
        handle
    }

    /// Cancel the task registered under `key`, if any.
    pub fn remove(&self, key: &str) {
        if let Some(runner) = locked(&self.runners).remove(key) {
            runner.abort();
            tracing::debug!(task = %key, "stopped interval");
        }
    }
}
