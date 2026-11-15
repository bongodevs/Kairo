//! Stable plugin boundary built on `abi_stable`.
//!
//! The root module is a prefix struct exported by every plugin library. It creates one
//! [`PluginInterface`] object per load. JSON remains the message format for `load` and `open`;
//! the object graph, buffers and errors are laid out by `abi_stable` instead of a hand-written ABI.

use abi_stable::StableAbi;
use abi_stable::library::RootModule;
use abi_stable::sabi_trait;
use abi_stable::sabi_types::VersionStrings;
use abi_stable::std_types::{RArc, RBox, RResult, RString};
use async_ffi::FfiFuture;

/// Version of the source-plugin contract. A host rejects a library that reports another value.
///
/// Version 2 adds async `load`, `open` and host `http` through [`FfiFuture`].
pub const ABI_VERSION: u32 = 2;

/// Log severity passed from a plugin to the host.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Severity of a plugin failure, matching the client-visible exception levels.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureSeverity {
    Common,
    Suspicious,
    Fault,
}

/// Failure returned across the plugin boundary.
#[repr(C)]
#[derive(StableAbi, Clone, Debug, PartialEq, Eq)]
pub struct PluginFailure {
    pub severity: FailureSeverity,
    pub message: RString,
    pub unavailable: bool,
}

impl PluginFailure {
    #[must_use]
    pub fn new(severity: FailureSeverity, message: impl Into<RString>, unavailable: bool) -> Self {
        Self { severity, message: message.into(), unavailable }
    }

    #[must_use]
    pub fn common(message: impl Into<RString>) -> Self {
        Self::new(FailureSeverity::Common, message, false)
    }

    #[must_use]
    pub fn suspicious(message: impl Into<RString>) -> Self {
        Self::new(FailureSeverity::Suspicious, message, false)
    }

    #[must_use]
    pub fn fault(message: impl Into<RString>) -> Self {
        Self::new(FailureSeverity::Fault, message, false)
    }

    #[must_use]
    pub fn unavailable(message: impl Into<RString>) -> Self {
        Self::new(FailureSeverity::Suspicious, message, true)
    }
}

impl From<crate::model::PluginError> for PluginFailure {
    fn from(error: crate::model::PluginError) -> Self {
        let severity = match error.severity {
            crate::model::Severity::Common => FailureSeverity::Common,
            crate::model::Severity::Suspicious => FailureSeverity::Suspicious,
            crate::model::Severity::Fault => FailureSeverity::Fault,
        };
        Self { severity, message: error.message.into(), unavailable: error.unavailable }
    }
}

impl From<PluginFailure> for crate::model::PluginError {
    fn from(failure: PluginFailure) -> Self {
        let severity = match failure.severity {
            FailureSeverity::Common => crate::model::Severity::Common,
            FailureSeverity::Suspicious => crate::model::Severity::Suspicious,
            FailureSeverity::Fault => crate::model::Severity::Fault,
        };
        Self { severity, message: failure.message.into_string(), unavailable: failure.unavailable }
    }
}

/// Host services available to a plugin during a call.
///
/// The host owns the implementation; the plugin receives an [`RArc`] of this object when it is
/// created. Methods must be thread-safe and free of unwinding panics.
#[sabi_trait]
pub trait HostService: Send + Sync {
    fn log(&self, level: LogLevel, message: RString);

    /// Run a detached task on the host's runtime.
    ///
    /// The raw `tokio::runtime::Handle` never crosses the ABI. The host captures it when the
    /// plugin is created and forwards work to it through this FFI-safe method.
    fn spawn(&self, future: FfiFuture<()>);

    /// Run one HTTP request on the host's async runtime.
    #[sabi(last_prefix_field)]
    fn http(&self, request: RString) -> FfiFuture<RResult<RString, PluginFailure>>;
}

/// Owned host service object passed to a plugin.
pub type HostServiceBox = HostService_TO<'static, RBox<()>>;

/// One initialized plugin object.
///
/// Requests and results stay JSON so the wire model can grow without changing the stable layout.
/// `shutdown` takes `&mut self`, which keeps it exclusive while calls may run concurrently.
#[sabi_trait]
pub trait PluginInterface: Send + Sync {
    fn load(&self, request: RString) -> FfiFuture<RResult<RString, PluginFailure>>;

    fn open(&self, request: RString) -> FfiFuture<RResult<RString, PluginFailure>>;

    #[sabi(last_prefix_field)]
    fn shutdown(&mut self);
}

/// Owned plugin object returned by the root module.
pub type PluginBox = PluginInterface_TO<'static, RBox<()>>;

/// Manifest and object created by one successful plugin initialization.
#[repr(C)]
#[derive(StableAbi)]
pub struct PluginInstance {
    /// JSON form of `kairo_plugin_api::Manifest`.
    pub manifest: RString,
    pub plugin: PluginBox,
}

/// Root module exported by a plugin library.
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = PluginRootRef)))]
pub struct PluginRoot {
    /// Version reported by the plugin implementation.
    pub abi_version: extern "C" fn() -> u32,

    /// Parse the opaque plugin config and create one plugin object.
    #[sabi(last_prefix_field)]
    pub create:
        extern "C" fn(RArc<HostServiceBox>, RString) -> RResult<PluginInstance, PluginFailure>,
}

impl RootModule for PluginRootRef {
    abi_stable::declare_root_module_statics! {PluginRootRef}

    const BASE_NAME: &'static str = "kairo_plugin";
    const NAME: &'static str = "kairo plugin";
    const VERSION_STRINGS: VersionStrings = abi_stable::package_version_strings!();
}

#[cfg(test)]
#[path = "../tests/abi.rs"]
mod tests;
