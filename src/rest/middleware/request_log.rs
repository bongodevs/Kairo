//! One request line shaped like the reference's request log.
//!
//! The payload is captured by teeing the request body while the handler reads it, so a request
//! that never reaches a body extractor logs no payload and the handler always sees every byte.
//! When the feature is disabled the router installs no layer at all.

use std::net::{IpAddr, SocketAddr};

use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::app::AppState;

/// Matches the framework's default JSON body limit; a larger body is rejected before a handler.
const MAX_BODY: usize = 2 * 1024 * 1024;

/// Keys whose payload values must never reach a log line.
const SENSITIVE_KEYS: &[&str] = &["password", "secret", "token", "authorization", "apikey"];

pub(crate) async fn middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let config = state.request_log.clone();
    if !config.enabled {
        return next.run(req).await;
    }

    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let query = config.include_query_string.then(|| req.uri().query().unwrap_or("").to_owned());
    let client = config
        .include_client_info
        .then(|| req.extensions().get::<ConnectInfo<SocketAddr>>().map(|info| info.0.ip()))
        .flatten();
    let headers = config.include_headers.then(|| log_headers(req.headers()));

    let parts = Parts {
        method: method.as_str(),
        path: &path,
        query: query.as_deref(),
        client,
        headers: headers.as_deref(),
        payload: None,
    };
    if config.before_request {
        tracing::info!(">> {}", parts.render());
    }

    let (parts, body) = req.into_parts();
    let (body, payload) = if config.include_payload {
        match to_bytes(body, MAX_BODY).await {
            Ok(bytes) => {
                (Body::from(bytes.clone()), Some(redact_payload(&bytes, config.max_payload_length)))
            }
            Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        }
    } else {
        (body, None)
    };
    let response = next.run(Request::from_parts(parts, body)).await;

    tracing::info!(
        "{}",
        Parts {
            method: method.as_str(),
            path: &path,
            query: query.as_deref(),
            client,
            headers: headers.as_deref(),
            payload: payload.as_deref(),
        }
        .render()
    );
    response
}

struct Parts<'a> {
    method: &'a str,
    path: &'a str,
    query: Option<&'a str>,
    client: Option<IpAddr>,
    headers: Option<&'a str>,
    payload: Option<&'a str>,
}

impl Parts<'_> {
    fn render(&self) -> String {
        let mut line = format!("{} {}", self.method, self.path);
        if let Some(query) = self.query.filter(|query| !query.is_empty()) {
            line.push('?');
            line.push_str(query);
        }
        if let Some(client) = self.client {
            line.push_str(", client=");
            line.push_str(&client.to_string());
        }
        if let Some(headers) = self.headers {
            line.push_str(", headers=");
            line.push_str(headers);
        }
        if let Some(payload) = self.payload.filter(|payload| !payload.is_empty()) {
            line.push_str(", payload=");
            line.push_str(payload);
        }
        line
    }
}

fn log_headers(headers: &HeaderMap) -> String {
    let mut line = String::from("{");
    for (name, value) in headers {
        if line.len() > 1 {
            line.push_str(", ");
        }
        line.push_str(name.as_str());
        line.push_str("=[");
        if is_sensitive_header(name.as_str()) {
            line.push_str("redacted");
        } else {
            line.push_str(value.to_str().unwrap_or("[binary]"));
        }
        line.push(']');
    }
    line.push('}');
    line
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "set-cookie" | "proxy-authorization"
    )
}

/// Redact known secret keys, then cut the payload at the configured byte cap.
fn redact_payload(bytes: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = match serde_json::from_str::<Value>(&text) {
        Ok(mut value) => {
            redact_value(&mut value);
            value.to_string()
        }
        Err(_) => text.into_owned(),
    };
    truncate(&text, limit)
}

fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_sensitive_key(key) {
                    *value = Value::String(String::from("[redacted]"));
                } else {
                    redact_value(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_value),
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SENSITIVE_KEYS.iter().any(|sensitive| key.contains(sensitive))
}

/// Cut at a character boundary and escape control characters so one request stays one line.
fn truncate(text: &str, limit: usize) -> String {
    let mut escaped = String::with_capacity(text.len().min(limit));
    let mut used = 0;
    for character in text.chars() {
        let width = character.len_utf8();
        if used + width > limit {
            break;
        }
        used += width;
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
#[path = "../../tests/request_log.rs"]
mod tests;
