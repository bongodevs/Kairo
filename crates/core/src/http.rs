//! HTTP details shared by the workspace's first-party clients.

use std::time::Duration;

/// Default budget for one outbound HTTP request.
///
/// The plugin host client, the update check and the provider JSON client all abandon a stalled
/// request after this delay.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// `User-Agent` sent by every HTTP client the workspace builds itself.
///
/// The version comes from the workspace package version, so the plugin host, the media client and
/// the update check all advertise the same identity.
pub const USER_AGENT: &str = concat!("kairo/", env!("CARGO_PKG_VERSION"));
