//! Error responses and the middleware that renders them as the v4 JSON error body.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// A failed request: a status, a client-facing message, and an optional technical trace.
#[derive(Debug, Clone)]
pub struct RestError {
    pub status: StatusCode,
    pub message: String,
    /// Sent back only when the request asked for it with `?trace`.
    pub trace: Option<String>,
}

/// What every handler returns.
pub type RestResult<T> = Result<T, RestError>;

impl RestError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            trace: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    /// A `400` built from a [`player::FriendlyException`], with its cause as the trace.
    pub fn from_friendly(error: &player::FriendlyException) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.message.clone(),
            trace: error.cause.clone(),
        }
    }
}

// Rides along in the response extensions so the middleware, which alone knows the request path,
// can render the body once.
#[derive(Clone)]
struct ErrorInfo {
    message: String,
    trace: Option<String>,
}

impl IntoResponse for RestError {
    fn into_response(self) -> Response {
        let mut response = self.status.into_response();
        response.extensions_mut().insert(ErrorInfo {
            message: self.message,
            trace: self.trace,
        });
        response
    }
}

// The v4 error body.
#[derive(Debug, Serialize)]
struct ErrorBody {
    timestamp: i64,
    status: u16,
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace: Option<String>,
    message: String,
    path: String,
}

/// Render every 4xx and 5xx response as the JSON error body. Sits outermost, so responses the
/// router produces by itself are covered as well.
pub async fn error_body_middleware(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let trace_requested = request
        .uri()
        .query()
        .map(|q| query_flag(q, "trace"))
        .unwrap_or(false);

    let response = next.run(request).await;
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }

    // A handler-produced error brings its own message and trace, a router 404 or 405 does not.
    let info = response.extensions().get::<ErrorInfo>().cloned();
    let message = info
        .as_ref()
        .map(|i| i.message.clone())
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("Error").to_string());
    let trace = if trace_requested {
        info.and_then(|i| i.trace)
    } else {
        None
    };

    let body = ErrorBody {
        timestamp: now_millis(),
        status: status.as_u16(),
        error: status.canonical_reason().unwrap_or("Error").to_string(),
        trace,
        message,
        path,
    };

    let json = serde_json::to_vec(&body).unwrap_or_default();
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(json),
    )
        .into_response()
}

// Spring's `ON_PARAM` triggers on the parameter's presence, so `?trace`, `?trace=true` and even
// `?trace=false` all request the trace.
fn query_flag(query: &str, key: &str) -> bool {
    query
        .split('&')
        .any(|pair| pair.split('=').next() == Some(key))
}

pub use crate::utils::time::now_millis;
