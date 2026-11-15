#![deny(unsafe_code)]

//! Shared stable boundary, messages and SDK for native source plugins.
//!
//! The `sdk` feature is what a plugin author compiles against. The `host` feature is the safe
//! library wrapper the server uses. `abi_stable` owns the object layouts, allocation and loading;
//! no raw pointer or allocator crosses the boundary by hand.

#[allow(unsafe_code, non_local_definitions)]
pub mod abi;
#[cfg(feature = "host")]
pub mod host;
pub mod model;
#[cfg(feature = "sdk")]
#[doc(hidden)]
pub mod sdk;

#[doc(hidden)]
pub use abi_stable::export_root_module;
pub use async_ffi::{FfiFuture, FutureExt};

/// File extension of every plugin binary, on every OS.
///
/// The contents stay native: the host checks the header before it loads the file.
pub const EXTENSION: &str = "kai";

pub use abi::{
    ABI_VERSION, FailureSeverity, HostService, HostServiceBox, LogLevel, PluginBox, PluginFailure,
    PluginInstance, PluginInterface, PluginRoot, PluginRootRef,
};
#[cfg(feature = "host")]
pub use host::{HostCallbacks, HostError, HostFuture, LoadedPlugin, MAX_RESPONSE_BYTES};
pub use model::{
    Headers, HttpMethod, HttpRequest, HttpResponse, LoadRequest, LoadResult, MAX_PLUGIN_DATA_BYTES,
    Manifest, Open, OpenRequest, Playlist, PluginError, Severity, Source, Track, TrackInfo,
};
#[cfg(feature = "sdk")]
pub use sdk::{Host, HttpCall, Plugin};
