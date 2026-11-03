//! Classification of load failures from structured cause tokens.
//!
//! The engine reports [`player::FriendlyException`] with a client-safe `message`
//! and a technical `cause`. Since player v0.1.9 the cause of a transport failure
//! carries machine-readable tokens (`timeout=true connect=false ... | caused by
//! ... | io kind=TimedOut os=Some(110) ...`) produced by
//! `player::tools::http_config::send_error_detail`, which walks the reqwest error
//! source chain instead of keeping bare `Display`. This module parses those tokens
//! rather than substring-matching human text.
//!
//! Causes from older sources, or from non-transport failures, carry no tokens and
//! classify as `unknown` — an honest unknown, not a guess.

/// Structured reading of one failure cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CauseClass {
    /// `timeout=true`: deadline exceeded (connect or total).
    Timeout,
    /// `connect=true`: TCP/TLS establishment failed (refused, reset, unreachable).
    Connect,
    /// `io kind=...`: the OS error kind, e.g. `TimedOut`, `ConnectionRefused`,
    /// `AddrNotAvailable` (EMFILE-adjacent bind failures surface here too).
    Io(&'static str),
    /// Body/decode failure after connecting (`body=true`, `decode=true`).
    Body,
    /// No transport tokens present: a non-transport failure or a truncated cause
    /// from an older source version. Deliberately not inferred further.
    Unknown,
}

impl CauseClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Io(kind) => kind,
            Self::Body => "body",
            Self::Unknown => "unknown",
        }
    }
}

/// Parse the structured tokens in `err.cause`. Never inspects `message`.
pub(crate) fn classify_detailed(err: &player::FriendlyException) -> CauseClass {
    let cause = err.cause.as_deref().unwrap_or("");
    if token_is(cause, "timeout", "true") {
        return CauseClass::Timeout;
    }
    if token_is(cause, "connect", "true") {
        return CauseClass::Connect;
    }
    if token_is(cause, "body", "true") || token_is(cause, "decode", "true") {
        return CauseClass::Body;
    }
    if let Some(kind) = token_value(cause, "io kind") {
        return CauseClass::Io(canonical_io_kind(kind));
    }
    CauseClass::Unknown
}

/// Backwards-compatible one-line class for logs.
pub fn classify(err: &player::FriendlyException) -> &'static str {
    classify_detailed(err).as_str()
}

// `key=value` with single-space separators, as emitted by `send_error_detail`.
// Values containing spaces (`io kind=TimedOut`) are matched on the full key.
fn token_is(haystack: &str, key: &str, value: &str) -> bool {
    token_value(haystack, key).is_some_and(|v| v == value)
}

fn token_value<'a>(haystack: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    haystack.find(&needle).map(|at| {
        haystack[at + needle.len()..]
            .split([' ', '|'])
            .next()
            .unwrap_or("")
    })
}

// `io::ErrorKind` debug names are stable API; map the common ones to short
// classes and pass the rest through verbatim.
fn canonical_io_kind(kind: &str) -> &'static str {
    match kind {
        "TimedOut" => "timeout",
        "ConnectionRefused" => "connection-refused",
        "ConnectionReset" => "connection-reset",
        "ConnectionAborted" => "connection-aborted",
        "NotConnected" => "not-connected",
        "AddrNotAvailable" => "addr-not-available",
        "AddrInUse" => "addr-in-use",
        "BrokenPipe" => "broken-pipe",
        "UnexpectedEof" => "unexpected-eof",
        _ => "io-other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use player::error::Severity;

    fn friendly(cause: &str) -> player::FriendlyException {
        player::FriendlyException::new("HTTP request failed.", Severity::Suspicious)
            .with_cause(cause)
    }

    #[test]
    fn timeout_token_wins_over_connect() {
        let err = friendly("timeout=true connect=true body=false decode=false request=true status=None | caused by foo");
        assert_eq!(classify(&err), "timeout");
    }

    #[test]
    fn connect_token_classifies() {
        let err = friendly("timeout=false connect=true body=false decode=false request=true status=None | caused by foo");
        assert_eq!(classify(&err), "connect");
    }

    #[test]
    fn io_kind_maps() {
        let err = friendly("timeout=false connect=false body=false decode=false request=true status=None | caused by io kind=ConnectionRefused os=Some(111) msg=foo");
        assert_eq!(classify(&err), "connection-refused");
    }

    #[test]
    fn bare_display_without_tokens_is_unknown_not_guessed() {
        // Pre-v0.1.9 causes and non-transport failures carry no tokens.
        let err = friendly("error sending request for url (https://example.com/)");
        assert_eq!(classify(&err), "unknown");
        let err = player::FriendlyException::new("No matches.", Severity::Common);
        assert_eq!(classify(&err), "unknown");
    }
}
