//! Safe ownership wrapper around one loaded `abi_stable` plugin library.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use abi_stable::library::{LibraryError, lib_header_from_path};
use abi_stable::sabi_trait::prelude::TD_Opaque;
use abi_stable::std_types::{RArc, RErr, ROk, RResult, RString};
use async_ffi::{FfiFuture, FutureExt as _};
use futures_util::FutureExt as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

use crate::ABI_VERSION;
use crate::abi::{
    HostService, HostService_TO, HostServiceBox, LogLevel, PluginBox, PluginFailure, PluginRootRef,
};
use crate::model::{HttpRequest, HttpResponse, Manifest, PluginError};

/// Largest JSON body accepted from one plugin call.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// A host callback future. It runs on the runtime that polls the plugin call.
pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct HostBridge {
    callbacks: Arc<dyn HostCallbacks>,
    runtime: Option<tokio::runtime::Handle>,
}

impl HostService for HostBridge {
    fn log(&self, level: LogLevel, message: RString) {
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            self.callbacks.log(level, message.as_str());
        }));
    }

    fn spawn(&self, future: FfiFuture<()>) {
        let runtime = self.runtime.clone().or_else(|| tokio::runtime::Handle::try_current().ok());
        match runtime {
            Some(runtime) => {
                runtime.spawn(future);
            }
            None => self
                .callbacks
                .log(LogLevel::Warn, "host runtime is unavailable; dropped a plugin task"),
        }
    }

    fn http(&self, request: RString) -> FfiFuture<RResult<RString, PluginFailure>> {
        let callbacks = Arc::clone(&self.callbacks);
        async move {
            let outcome = AssertUnwindSafe(async move {
                let request: HttpRequest = match serde_json::from_str(request.as_str()) {
                    Ok(request) => request,
                    Err(error) => {
                        return RErr(PluginFailure::suspicious(format!(
                            "invalid host HTTP request: {error}"
                        )));
                    }
                };
                let response = match callbacks.http(request).await {
                    Ok(response) => response,
                    Err(error) => return RErr(error.into()),
                };
                match serde_json::to_string(&response) {
                    Ok(body) => ROk(RString::from(body)),
                    Err(_) => RErr(PluginFailure::fault("failed to encode host HTTP response")),
                }
            })
            .catch_unwind()
            .await;
            match outcome {
                Ok(response) => response,
                Err(_) => RErr(PluginFailure::fault("host HTTP callback panicked")),
            }
        }
        .into_ffi()
    }
}

/// Failure while loading or calling a native plugin.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HostError {
    #[error("failed to load plugin library {path}")]
    Library {
        path: PathBuf,
        #[source]
        source: LibraryError,
    },
    #[error("plugin ABI {actual} does not match host ABI {expected}")]
    Abi { expected: u32, actual: u32 },
    #[error("failed to encode plugin {operation} request")]
    RequestJson {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("plugin {operation} returned invalid JSON")]
    ResponseJson {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("plugin {operation} response exceeds {max} bytes")]
    ResponseTooLarge { operation: &'static str, max: usize },
    #[error("plugin {operation} failed: {error}")]
    Call {
        operation: &'static str,
        #[source]
        error: PluginError,
    },
    #[error("unknown plugin method {method:?}")]
    UnknownMethod { method: String },
    #[error("plugin lifecycle lock is poisoned")]
    Poisoned,
    #[error("plugin has shut down")]
    Shutdown,
}

/// Thread-safe services exposed to plugin code.
///
/// `http` returns a future instead of blocking a thread. The future is polled by the same runtime
/// that is waiting for the plugin call, so the host needs no second runtime and no `block_on`.
pub trait HostCallbacks: Send + Sync + 'static {
    fn log(&self, level: LogLevel, message: &str);
    fn http<'a>(
        &'a self,
        request: HttpRequest,
    ) -> HostFuture<'a, Result<HttpResponse, PluginError>>;
}

struct Lifecycle {
    plugin: Option<PluginBox>,
    in_flight: usize,
    closed: bool,
}

/// One initialized plugin whose library stays mapped for its whole lifetime.
pub struct LoadedPlugin {
    manifest: Manifest,
    lifecycle: Mutex<Lifecycle>,
    idle: Condvar,
    _host: RArc<HostServiceBox>,
}

impl LoadedPlugin {
    pub fn load(
        path: impl AsRef<Path>,
        callbacks: Arc<dyn HostCallbacks>,
        config: &Value,
    ) -> Result<Self, HostError> {
        let path = path.as_ref();
        let header = lib_header_from_path(path)
            .map_err(|source| HostError::Library { path: path.to_owned(), source })?;
        let root = header
            .init_root_module::<PluginRootRef>()
            .map_err(|source| HostError::Library { path: path.to_owned(), source })?;
        check_abi((root.abi_version())())?;

        let service: HostServiceBox = HostService_TO::from_value(
            HostBridge { callbacks, runtime: tokio::runtime::Handle::try_current().ok() },
            TD_Opaque,
        );
        let host = RArc::new(service);
        let config = encode_json("init", config)?;
        let mut instance = match (root.create())(RArc::clone(&host), config.into()) {
            ROk(instance) => instance,
            RErr(error) => return Err(call_error("init", error)),
        };
        let manifest: Manifest = match decode_json("init", instance.manifest.as_str()) {
            Ok(manifest) => manifest,
            Err(error) => {
                instance.plugin.shutdown();
                return Err(error);
            }
        };
        if let Err(error) = check_abi(manifest.abi) {
            instance.plugin.shutdown();
            return Err(error);
        }

        Ok(Self {
            manifest,
            lifecycle: Mutex::new(Lifecycle {
                plugin: Some(instance.plugin),
                in_flight: 0,
                closed: false,
            }),
            idle: Condvar::new(),
            _host: host,
        })
    }

    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Call one plugin method and await its result.
    ///
    /// The future borrows nothing from the plugin object after the call starts: the FfiFuture owns
    /// the state it needs, so the lifecycle lock is released before awaiting.
    pub async fn call<I, O>(&self, method: &str, request: &I) -> Result<O, HostError>
    where
        I: Serialize,
        O: DeserializeOwned,
    {
        let request = encode_json("call", request)?;
        let response = {
            let mut lifecycle = self.lifecycle();
            if lifecycle.closed {
                return Err(HostError::Shutdown);
            }
            let plugin = lifecycle.plugin.as_ref().ok_or(HostError::Shutdown)?;
            let response = match method {
                "source.load" => plugin.load(request.into()),
                "track.open" => plugin.open(request.into()),
                method => return Err(HostError::UnknownMethod { method: method.to_owned() }),
            };
            lifecycle.in_flight += 1;
            response
        };
        let _in_flight = InFlight { loaded: self };
        let body = match response.await {
            ROk(body) => body,
            RErr(error) => return Err(call_error("call", error)),
        };
        decode_json("call", body.as_str())
    }

    /// Stop accepting calls, wait for in-flight calls, then run the plugin's shutdown hook.
    ///
    /// Call this from a blocking context when calls may still be running (the manager uses
    /// `spawn_blocking`). Dropping the last `Arc<LoadedPlugin>` already guarantees no call can be
    /// in flight, so the `Drop` implementation never waits.
    pub fn shutdown(&self) {
        let mut lifecycle = self.lifecycle();
        lifecycle.closed = true;
        while lifecycle.in_flight > 0 {
            lifecycle = self.idle.wait(lifecycle).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if let Some(mut plugin) = lifecycle.plugin.take() {
            drop(lifecycle);
            plugin.shutdown();
        }
    }

    fn lifecycle(&self) -> MutexGuard<'_, Lifecycle> {
        self.lifecycle.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct InFlight<'a> {
    loaded: &'a LoadedPlugin,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let mut lifecycle = self.loaded.lifecycle();
        lifecycle.in_flight = lifecycle.in_flight.saturating_sub(1);
        if lifecycle.in_flight == 0 {
            self.loaded.idle.notify_all();
        }
    }
}

impl Drop for LoadedPlugin {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn encode_json<T: Serialize>(operation: &'static str, value: &T) -> Result<String, HostError> {
    serde_json::to_string(value).map_err(|source| HostError::RequestJson { operation, source })
}

fn decode_json<T: DeserializeOwned>(operation: &'static str, body: &str) -> Result<T, HostError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(HostError::ResponseTooLarge { operation, max: MAX_RESPONSE_BYTES });
    }
    serde_json::from_str(body).map_err(|source| HostError::ResponseJson { operation, source })
}

fn call_error(operation: &'static str, failure: PluginFailure) -> HostError {
    HostError::Call { operation, error: failure.into() }
}

fn check_abi(actual: u32) -> Result<(), HostError> {
    if actual == ABI_VERSION {
        Ok(())
    } else {
        Err(HostError::Abi { expected: ABI_VERSION, actual })
    }
}

#[cfg(test)]
#[path = "../tests/host.rs"]
mod tests;
