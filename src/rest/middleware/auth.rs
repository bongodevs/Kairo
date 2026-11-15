//! Password authorization for the REST API, matching the reference's wire behavior: a raw
//! `Authorization: <token>` header (not `Bearer`), `401` when the header is absent and `403` when it
//! is present but wrong. No REST route is anonymous except `/error` and the (future) metrics
//! endpoint, and an unset password denies every request. The WebSocket handshake authorizes
//! separately (see `ws`), as it does in the reference.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Authz {
    Allow,
    /// No `Authorization` header at all.
    Unauthorized,
    /// Header present but it does not match the configured password (or none is set).
    Forbidden,
}

/// Pure decision, kept separate so it can be tested without a live request.
pub(super) fn authorize(authorization: Option<&str>, password: Option<&str>) -> Authz {
    let Some(given) = authorization else {
        return Authz::Unauthorized;
    };
    match password {
        // Deliberate hardening over the reference, which uses `String.equals`: a secret
        // compared with an early-return comparison leaks how many leading bytes matched.
        Some(expected) if secret_eq(given.as_bytes(), expected.as_bytes()) => Authz::Allow,
        _ => Authz::Forbidden,
    }
}

pub(crate) fn secret_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn authorization_header(req: &Request) -> Option<&str> {
    req.headers().get(AUTHORIZATION).and_then(|v| v.to_str().ok())
}

pub(crate) async fn middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let given = authorization_header(&req);
    match authorize(given, state.password()) {
        Authz::Allow => next.run(req).await,
        Authz::Unauthorized => {
            tracing::warn!(method = %req.method(), path = %req.uri().path(), "authorization missing");
            StatusCode::UNAUTHORIZED.into_response()
        }
        Authz::Forbidden => {
            tracing::warn!(method = %req.method(), path = %req.uri().path(), "authorization failed");
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

#[cfg(test)]
#[path = "../../tests/auth.rs"]
mod tests;
