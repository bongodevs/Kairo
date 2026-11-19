use std::time::Duration;

/// Default budget for one outbound HTTP request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// User-Agent sent by every HTTP client the workspace builds itself.
pub const USER_AGENT: &str = concat!("kairo/", env!("CARGO_PKG_VERSION"));
