//! Bounded JSON client and outbound policy for declarative providers.
//!
//! [`Api`] is retained with the provider framework: the bundled service providers now make their
//! HTTP calls through the plugin host instead, so the API is covered by the `source_framework`
//! tests only. The client policy, direct-reference helpers and the local-address provider are still
//! used by the HTTP and local transports.

mod api;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use reqwest::redirect::Policy;

use kairo_core::SourceReference;

use crate::source::error::SourceError;
use crate::source::proxy::ProxyConfig;

pub use api::{Api, Call};

/// Supplies the local address each outbound generic-HTTP request binds to. `None` means the
/// operating system chooses, as usual.
pub type LocalAddressProvider = Arc<dyn Fn() -> Option<IpAddr> + Send + Sync>;

/// Wrap a closure as a [`LocalAddressProvider`].
pub fn local_address_provider<F>(provider: F) -> LocalAddressProvider
where
    F: Fn() -> Option<IpAddr> + Send + Sync + 'static,
{
    Arc::new(provider)
}

/// Whether an identifier is a direct HTTP(S) or ICY stream.
pub(crate) fn is_direct(identifier: &str) -> bool {
    identifier.starts_with("http://")
        || identifier.starts_with("https://")
        || identifier.starts_with("icy://")
}

/// Build the outbound client policy shared by direct streams and provider APIs.
///
/// A zero connect or read timeout leaves that side of the request unbounded, matching the
/// reference's millisecond settings.
pub(crate) fn client(
    connect: Duration,
    read: Duration,
    redirect: Policy,
    proxy: Option<&ProxyConfig>,
    local_address: Option<IpAddr>,
) -> Result<Client, SourceError> {
    // The process may already have installed ring; keeping that provider is intentional.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = Client::builder()
        .redirect(redirect)
        .connect_timeout(connect)
        .read_timeout(read)
        .local_address(local_address)
        .user_agent(kairo_core::http::USER_AGENT);
    let builder = match proxy {
        Some(proxy) => proxy.apply(builder).map_err(|source| SourceError::HttpClient { source })?,
        None => builder,
    };
    builder.build().map_err(|source| SourceError::HttpClient { source })
}

/// Normalize a direct stream reference; `icy://` becomes `http://`.
pub(crate) fn direct_reference(reference: &SourceReference) -> Option<SourceReference> {
    let identifier = reference.identifier();
    if identifier.starts_with("http://") || identifier.starts_with("https://") {
        return Some(reference.clone());
    }
    identifier
        .strip_prefix("icy://")
        .map(|identifier| reference.replacing_identifier(format!("http://{identifier}")))
}
