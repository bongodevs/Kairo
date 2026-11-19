//! Safe plugin author API and generated root-module export.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use abi_stable::prefix_type::PrefixTypeTrait;
use abi_stable::sabi_trait::prelude::TD_Opaque;
use abi_stable::std_types::{RArc, RErr, ROk, RResult, RString};
use async_ffi::{FfiFuture, FutureExt as _};
use futures_util::FutureExt as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ABI_VERSION;
use crate::abi::{
    HostServiceBox, LogLevel, PluginBox, PluginFailure, PluginInstance, PluginInterface,
    PluginInterface_TO,
};
use crate::model::{
    Headers, HttpMethod, HttpRequest, HttpResponse, LoadRequest, LoadResult, Manifest, Open,
    OpenRequest, PluginError,
};

/// One plugin implemented by an author.
///
/// `load` and `open` are async and run on the host's runtime: awaiting a [`Host`] HTTP call simply
/// yields to that runtime, it never starts or blocks another one. The host may call `load` and
/// `open` concurrently. `init` is synchronous and runs before any call; `shutdown` runs after the
/// last call has finished.
pub trait Plugin: Default + Send + Sync + 'static {
    fn init(&mut self, host: &Host, config: Value) -> Result<Manifest, PluginError>;

    fn load(
        &self,
        host: &Host,
        request: LoadRequest,
    ) -> impl Future<Output = Result<LoadResult, PluginError>> + Send;

    fn open(
        &self,
        host: &Host,
        request: OpenRequest,
    ) -> impl Future<Output = Result<Open, PluginError>> + Send;

    fn shutdown(&mut self) {}
}

/// Host services available during a plugin call.
#[derive(Clone)]
pub struct Host {
    service: RArc<HostServiceBox>,
}

impl Host {
    fn new(service: RArc<HostServiceBox>) -> Self {
        Self { service }
    }

    pub fn log(&self, level: LogLevel, message: &str) {
        self.service.log(level, message.into());
    }

    pub fn trace(&self, message: &str) {
        self.log(LogLevel::Trace, message);
    }

    pub fn debug(&self, message: &str) {
        self.log(LogLevel::Debug, message);
    }

    pub fn info(&self, message: &str) {
        self.log(LogLevel::Info, message);
    }

    pub fn warn(&self, message: &str) {
        self.log(LogLevel::Warn, message);
    }

    pub fn error(&self, message: &str) {
        self.log(LogLevel::Error, message);
    }

    /// Run a detached task on the host runtime captured at plugin creation.
    ///
    /// This is the only way plugin code gets access to the host executor. The plugin never creates
    /// a runtime, so a `.so`/`.dll` cannot leave its own thread pool behind.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.service.spawn(future.into_ffi());
    }

    pub fn get(&self, url: impl Into<String>) -> HttpCall<'_> {
        self.request(HttpMethod::Get, url)
    }

    pub fn request(&self, method: HttpMethod, url: impl Into<String>) -> HttpCall<'_> {
        HttpCall {
            host: self,
            request: HttpRequest { method, url: url.into(), headers: Headers::new(), body: None },
        }
    }

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, PluginError> {
        let request = serde_json::to_string(&request).map_err(|error| {
            PluginError::fault(format!("failed to encode HTTP request: {error}"))
        })?;
        match self.service.http(request.into()).await {
            ROk(body) => serde_json::from_str(body.as_str()).map_err(|error| {
                PluginError::fault(format!("host returned an invalid HTTP response: {error}"))
            }),
            RErr(error) => Err(error.into()),
        }
    }
}

/// One HTTP request prepared for the host callback.
#[must_use]
pub struct HttpCall<'a> {
    host: &'a Host,
    request: HttpRequest,
}

impl HttpCall<'_> {
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.request.headers.insert(name.into(), value.into());
        self
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.request.body = Some(body.into());
        self
    }

    pub fn json_body<T: Serialize>(mut self, body: &T) -> Result<Self, PluginError> {
        self.request.body =
            Some(serde_json::to_string(body).map_err(|error| {
                PluginError::fault(format!("failed to encode JSON body: {error}"))
            })?);
        Ok(self)
    }

    pub async fn send(self) -> Result<HttpResponse, PluginError> {
        self.host.send(self.request).await
    }

    pub async fn json<T: DeserializeOwned>(self) -> Result<T, PluginError> {
        response_json(self.send().await?)
    }
}

fn response_json<T: DeserializeOwned>(response: HttpResponse) -> Result<T, PluginError> {
    if !(200..300).contains(&response.status) {
        return Err(PluginError::suspicious(format!(
            "HTTP request returned status {}",
            response.status
        )));
    }
    serde_json::from_str(&response.body)
        .map_err(|error| PluginError::suspicious(format!("response is not valid JSON: {error}")))
}

struct PluginAdapter<P: Plugin> {
    plugin: Arc<P>,
    host: Host,
}

impl<P: Plugin> PluginInterface for PluginAdapter<P> {
    fn load(&self, request: RString) -> FfiFuture<RResult<RString, PluginFailure>> {
        let plugin = Arc::clone(&self.plugin);
        let host = self.host.clone();
        async move {
            let outcome = AssertUnwindSafe(async move {
                let request: LoadRequest = decode("source.load", request)?;
                plugin.load(&host, request).await
            })
            .catch_unwind()
            .await;
            finish("source.load", outcome)
        }
        .into_ffi()
    }

    fn open(&self, request: RString) -> FfiFuture<RResult<RString, PluginFailure>> {
        let plugin = Arc::clone(&self.plugin);
        let host = self.host.clone();
        async move {
            let outcome = AssertUnwindSafe(async move {
                let request: OpenRequest = decode("track.open", request)?;
                plugin.open(&host, request).await
            })
            .catch_unwind()
            .await;
            finish("track.open", outcome)
        }
        .into_ffi()
    }

    fn shutdown(&mut self) {
        if let Some(plugin) = Arc::get_mut(&mut self.plugin) {
            let _ = catch_unwind(AssertUnwindSafe(|| plugin.shutdown()));
        }
    }
}

fn decode<I: DeserializeOwned>(
    operation: &'static str,
    request: RString,
) -> Result<I, PluginError> {
    serde_json::from_str(request.as_str())
        .map_err(|error| PluginError::suspicious(format!("invalid {operation} request: {error}")))
}

fn finish<O: Serialize>(
    operation: &'static str,
    outcome: Result<Result<O, PluginError>, Box<dyn std::any::Any + Send>>,
) -> RResult<RString, PluginFailure> {
    match outcome {
        Ok(Ok(value)) => match serde_json::to_string(&value) {
            Ok(body) => ROk(body.into()),
            Err(error) => {
                RErr(PluginError::fault(format!("encode {operation} response: {error}")).into())
            }
        },
        Ok(Err(error)) => RErr(error.into()),
        Err(_) => RErr(PluginFailure::fault(format!("plugin {operation} panicked"))),
    }
}

/// Build the root module for one plugin type. The export macro calls this.
#[doc(hidden)]
pub fn root<P: Plugin>() -> crate::PluginRootRef {
    crate::PluginRoot { abi_version, create: create::<P> }.leak_into_prefix()
}

extern "C" fn abi_version() -> u32 {
    ABI_VERSION
}

extern "C" fn create<P: Plugin>(
    host: RArc<HostServiceBox>,
    config: RString,
) -> RResult<PluginInstance, PluginFailure> {
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<PluginInstance, PluginError> {
        let config: Value = serde_json::from_str(config.as_str())
            .map_err(|error| PluginError::fault(format!("invalid plugin config: {error}")))?;
        let host = Host::new(host);
        let mut plugin = P::default();
        let manifest = match plugin.init(&host, config) {
            Ok(manifest) => manifest,
            Err(error) => {
                shutdown_plugin(&mut plugin);
                return Err(error);
            }
        };
        let manifest = match serde_json::to_string(&manifest) {
            Ok(manifest) => manifest,
            Err(error) => {
                shutdown_plugin(&mut plugin);
                return Err(PluginError::fault(format!("encode manifest: {error}")));
            }
        };
        let plugin: PluginBox = PluginInterface_TO::from_value(
            PluginAdapter { plugin: Arc::new(plugin), host },
            TD_Opaque,
        );
        Ok(PluginInstance { manifest: manifest.into(), plugin })
    }));
    match outcome {
        Ok(Ok(instance)) => ROk(instance),
        Ok(Err(error)) => RErr(error.into()),
        Err(_) => RErr(PluginFailure::fault("plugin init panicked")),
    }
}

fn shutdown_plugin<P: Plugin>(plugin: &mut P) {
    let _ = catch_unwind(AssertUnwindSafe(|| plugin.shutdown()));
}

/// Generates the `abi_stable` root module for one [`Plugin`] type.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[doc(hidden)]
        #[$crate::export_root_module]
        pub fn kairo_plugin_root() -> $crate::PluginRootRef {
            $crate::sdk::root::<$plugin>()
        }
    };
}

#[cfg(test)]
#[path = "../tests/sdk.rs"]
mod tests;
