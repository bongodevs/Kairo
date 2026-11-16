//! The source engine: ordered resolution, direct transports, codecs and the provider framework.

mod contract;
mod engine;
mod error;
mod extended_codec;
mod fallback;
mod framework;
mod http;
mod local;
mod manager;
mod net;
mod probe;
mod proxy;
mod refresh;
mod track;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use contract::*;
pub use engine::{LoadOutcome, Sources};
pub use error::{LoadError, ProviderError, SourceError};
pub use extended_codec::ExtendedTrackCodec;
pub use fallback::{fallback_suppressed, without_fallback};
pub use framework::{
    Kind, Load, Loader, Metrics, MetricsSnapshot, Opener, Provider, Request, Settings, Stream,
    read_config, read_with,
};
pub use http::{HTTP_SOURCE, HttpSource};
pub use local::{LOCAL_SOURCE, LocalSource};
pub use manager::{Config, ProviderManager, SourceCatalog, SourceConfig, SourceOptions};
pub use net::{Api, Call, LocalAddressProvider, local_address_provider};
pub use probe::{ProbeError, SourceProbe};
pub use proxy::ProxyConfig;
pub use track::{OpenFuture, Playback, TrackSource};

#[cfg(test)]
#[path = "../../tests/source_provider_manager.rs"]
mod provider_manager_tests;
#[cfg(test)]
#[path = "../../tests/source_refresh.rs"]
mod refresh_tests;
