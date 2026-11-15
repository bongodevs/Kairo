use std::backtrace::Backtrace;

use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::utils::time::now_millis;

// Oversized framework error text is discarded in favor of the HTTP status reason.
const MAX_ERROR_BODY: usize = 64 * 1024;

#[derive(Serialize)]
struct ErrorResponse {
    timestamp: u64,
    status: u16,
    error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace: Option<String>,
    message: String,
    path: String,
}

pub(super) async fn middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let include_trace = trace_requested(req.uri().query());
    let response = next.run(req).await;
    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }

    render(response, path, include_trace).await
}

async fn render(response: Response, path: String, include_trace: bool) -> Response {
    let (mut parts, body) = response.into_parts();
    parts.status = normalize_status(parts.status);
    let reason = parts.status.canonical_reason().unwrap_or("Unknown Error");
    let message = response_message(body, reason).await;
    let trace = include_trace.then(|| format!("{message}\n{}", Backtrace::force_capture()));
    let payload = ErrorResponse {
        timestamp: now_millis(),
        status: parts.status.as_u16(),
        error: reason,
        trace,
        message,
        path,
    };
    let body = match serde_json::to_vec(&payload) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(%error, "failed to serialize REST error response");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    parts.headers.remove(CONTENT_LENGTH);
    parts.headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Response::from_parts(parts, Body::from(body))
}

async fn response_message(body: Body, fallback: &str) -> String {
    match to_bytes(body, MAX_ERROR_BODY).await {
        Ok(body) => {
            let message = String::from_utf8_lossy(&body);
            let message = message.trim();
            if message.is_empty() { fallback.to_owned() } else { message.to_owned() }
        }
        Err(error) => {
            tracing::warn!(%error, "REST error body exceeded its formatting limit");
            fallback.to_owned()
        }
    }
}

fn normalize_status(status: StatusCode) -> StatusCode {
    if status == StatusCode::UNPROCESSABLE_ENTITY { StatusCode::BAD_REQUEST } else { status }
}

fn trace_requested(query: Option<&str>) -> bool {
    query.into_iter().flat_map(|query| query.split('&')).any(|pair| {
        pair.split_once('=')
            .is_some_and(|(name, value)| name == "trace" && value.eq_ignore_ascii_case("true"))
    })
}

#[cfg(test)]
#[path = "tests/errors.rs"]
mod tests;
