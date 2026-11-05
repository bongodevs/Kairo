//! Request logging middleware for the REST API.
//!
//! Each request is logged once it completes, as `METHOD /path?query, client=..., headers=[...],
//! payload=...`, with every segment gated by its own
//! [`RequestLoggingConfig`](crate::config::RequestLoggingConfig) flag. `before_request` adds a
//! second line, prefixed `>> `, before the handler runs. Sensitive headers are redacted.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, Method};
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::BodyExt;

use crate::node::AppState;

// Ceiling on how much of a request body is buffered to log it, set far above any real payload so a
// legitimate request is never truncated on the wire. The logged copy is cut to `max_payload_length`.
const MAX_BODY_BUFFER: usize = 16 * 1024 * 1024;

// Headers whose values are replaced with `[REDACTED]` in the `headers=` segment.
const REDACTED_HEADERS: [header::HeaderName; 4] = [
    header::AUTHORIZATION,
    header::PROXY_AUTHORIZATION,
    header::COOKIE,
    header::SET_COOKIE,
];

/// Log each REST request after it completes (and optionally before), per `logging.request.*`.
pub async fn request_logging(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let cfg = state.config().logging.request.clone();
    // Two gates: the flag is the operator asking for request logs, the level filter decides whether
    // anything would print them. Without the second, a filtered-out INFO still buffers every body
    // and formats every header for a line nobody reads.
    if !cfg.enabled || !tracing::enabled!(tracing::Level::INFO) {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let method = parts.method.clone();
    let path = parts.uri.path().to_string();
    let query = cfg
        .include_query_string
        .then(|| parts.uri.query().map(str::to_owned))
        .flatten();
    let client = cfg
        .include_client_info
        .then(|| {
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.to_string())
        })
        .flatten();
    let headers = cfg.include_headers.then(|| format_headers(&parts.headers));

    // Copy a bounded prefix of the body into a side buffer while the handler consumes it. The
    // handler always receives the original bytes, and an oversized or failing body is no longer
    // replaced with an empty one.
    let has_body = matches!(method, Method::POST | Method::PUT | Method::PATCH);
    let (body, captured) = if cfg.include_payload && has_body {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let body = body.map_frame(move |frame| {
            if let Some(data) = frame.data_ref() {
                let mut buffer = sink.lock().unwrap_or_else(|e| e.into_inner());
                if buffer.len() < MAX_BODY_BUFFER {
                    let room = MAX_BODY_BUFFER - buffer.len();
                    buffer.extend_from_slice(&data[..data.len().min(room)]);
                }
            }
            frame
        });
        (Body::new(body), Some(captured))
    } else {
        (body, None)
    };

    if cfg.before_request {
        tracing::info!(
            "{}",
            message(">> ", &method, &path, &query, &client, &headers, &None)
        );
    }

    let response = next.run(Request::from_parts(parts, body)).await;

    let payload = captured.and_then(|captured| {
        let bytes = captured.lock().unwrap_or_else(|e| e.into_inner());
        render_payload(&bytes, cfg.max_payload_length)
    });

    tracing::info!(
        "{}",
        message("", &method, &path, &query, &client, &headers, &payload)
    );

    response
}

// Assemble one log line from the segments that survived their flags.
fn message(
    prefix: &str,
    method: &Method,
    path: &str,
    query: &Option<String>,
    client: &Option<String>,
    headers: &Option<String>,
    payload: &Option<String>,
) -> String {
    let mut msg = String::with_capacity(64);
    msg.push_str(prefix);
    msg.push_str(method.as_str());
    msg.push(' ');
    msg.push_str(path);
    if let Some(query) = query {
        msg.push('?');
        msg.push_str(query);
    }
    if let Some(client) = client {
        msg.push_str(", client=");
        msg.push_str(client);
    }
    if let Some(headers) = headers {
        msg.push_str(", headers=");
        msg.push_str(headers);
    }
    if let Some(payload) = payload {
        msg.push_str(", payload=");
        msg.push_str(payload);
    }
    msg
}

// Render a body as lossy UTF-8, truncated to `max_len`, or `None` when it is empty.
fn render_payload(bytes: &[u8], max_len: usize) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let end = bytes.len().min(max_len);
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

// Format the headers as `[name:"value", ...]`, redacting the sensitive ones.
fn format_headers(headers: &HeaderMap) -> String {
    let rendered: Vec<String> = headers
        .iter()
        .map(|(name, value)| {
            let value = if REDACTED_HEADERS.contains(name) {
                "[REDACTED]"
            } else {
                value.to_str().unwrap_or("<binary>")
            };
            format!("{name}:\"{value}\"")
        })
        .collect();
    format!("[{}]", rendered.join(", "))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use axum::body::Bytes;
    use axum::extract::DefaultBodyLimit;
    use axum::middleware;
    use axum::routing::post;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{request_logging, MAX_BODY_BUFFER};
    use crate::node::AppState;

    async fn echo(body: Bytes) -> Bytes {
        body
    }

    #[derive(Clone)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for LogBuffer {
        type Writer = LogBuffer;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// A body above the capture ceiling must still reach the handler byte for byte, and the logged
    /// copy must be the configured prefix. Before the tee, the middleware replaced the body with an
    /// empty one as soon as `to_bytes` hit its cap.
    #[test]
    fn an_oversized_payload_reaches_the_handler_and_logs_a_prefix() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(LogBuffer(Arc::clone(&logs)))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("current-thread runtime");
            runtime.block_on(async {
                let app = Router::new()
                    .route("/echo", post(echo))
                    .layer(DefaultBodyLimit::disable())
                    .layer(middleware::from_fn_with_state(
                        AppState::test_state(),
                        request_logging,
                    ));

                const CHUNK: usize = 1024 * 1024;
                const CHUNKS: usize = MAX_BODY_BUFFER / CHUNK + 1;
                // No Content-Length: a chunked stream is the case the old buffering path broke.
                let stream = futures_util::stream::iter(
                    (0..CHUNKS).map(|i| Ok::<_, std::io::Error>(Bytes::from(vec![i as u8; CHUNK]))),
                );
                let response = app
                    .oneshot(
                        axum::http::Request::builder()
                            .method("POST")
                            .uri("/echo")
                            .body(axum::body::Body::from_stream(stream))
                            .expect("request"),
                    )
                    .await
                    .expect("response");

                let body = response
                    .into_body()
                    .collect()
                    .await
                    .expect("body")
                    .to_bytes();
                assert_eq!(body.len(), CHUNK * CHUNKS);
                for i in 0..CHUNKS {
                    assert_eq!(body[i * CHUNK], i as u8, "chunk {i} changed");
                }
            });
        });

        let logs = logs.lock().unwrap_or_else(|e| e.into_inner());
        let marker = b", payload=";
        let at = logs
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("the request log carries a payload");
        let logged = &logs[at + marker.len()..];
        let end = logged
            .iter()
            .position(|&byte| byte == b'\n')
            .unwrap_or(logged.len());
        assert_eq!(end, 10_000, "the logged copy is cut to max_payload_length");
    }
}
