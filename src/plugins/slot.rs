use std::future::Future;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use kairo_plugin_api::{HostError, LoadedPlugin, Severity};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;

/// How long one plugin call may wait for a permit and then run.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Calls allowed inside one plugin at the same time.
const MAX_CONCURRENT_CALLS: usize = 16;

/// The hot-swappable handle every adapter reads at the start of a call.
pub(super) struct PluginSlot {
    current: RwLock<Arc<LoadedPlugin>>,
    gate: CallGate,
}

impl PluginSlot {
    pub(super) fn new(plugin: Arc<LoadedPlugin>) -> Self {
        Self { current: RwLock::new(plugin), gate: CallGate::new() }
    }

    pub(super) fn plugin(&self) -> Arc<LoadedPlugin> {
        self.read().clone()
    }

    pub(super) fn replace(&self, plugin: Arc<LoadedPlugin>) -> Arc<LoadedPlugin> {
        std::mem::replace(&mut *self.write(), plugin)
    }

    /// Run one method under the shared deadline and concurrency cap.
    pub(super) async fn call<I, O>(&self, method: &str, request: &I) -> Result<O, CallError>
    where
        I: Serialize,
        O: DeserializeOwned,
    {
        self.gate.run(async { self.plugin().call(method, request).await }).await
    }

    fn read(&self) -> RwLockReadGuard<'_, Arc<LoadedPlugin>> {
        self.current.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Arc<LoadedPlugin>> {
        self.current.write().unwrap_or_else(PoisonError::into_inner)
    }
}

struct CallGate {
    timeout: Duration,
    permits: Semaphore,
}

impl CallGate {
    fn new() -> Self {
        Self::with_timeout(CALL_TIMEOUT)
    }

    fn with_timeout(timeout: Duration) -> Self {
        Self { timeout, permits: Semaphore::new(MAX_CONCURRENT_CALLS) }
    }

    async fn run<T>(
        &self,
        call: impl Future<Output = Result<T, HostError>>,
    ) -> Result<T, CallError> {
        let guarded = async {
            let _permit = self.permits.acquire().await.map_err(|_| HostError::Shutdown)?;
            call.await.map_err(CallError::Host)
        };
        match tokio::time::timeout(self.timeout, guarded).await {
            Ok(result) => result,
            Err(_) => Err(CallError::Timeout),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CallError {
    #[error("plugin call timed out after {} s", CALL_TIMEOUT.as_secs())]
    Timeout,
    #[error(transparent)]
    Host(#[from] HostError),
}

impl CallError {
    /// A timeout or a broken protocol means the plugin is sick; a rejected request does not.
    pub(super) fn counted(&self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Host(HostError::Call { error, .. }) => {
                error.unavailable || error.severity == Severity::Fault
            }
            Self::Host(
                HostError::ResponseJson { .. }
                | HostError::ResponseTooLarge { .. }
                | HostError::UnknownMethod { .. },
            ) => true,
            Self::Host(_) => false,
        }
    }
}

#[cfg(test)]
#[path = "../tests/plugins_slot.rs"]
mod tests;
