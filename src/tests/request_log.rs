use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::{redact_payload, truncate};
use crate::config::RequestLoggingConfig;
use crate::test_support::{
    LogCapture as Capture, app_state, capture_subscriber as subscriber, request_with_headers,
    spawn_server,
};

#[test]
fn redaction_and_truncation_never_leak_a_secret() {
    let text = redact_payload(
        br#"{"password":"hunter2","nested":{"secretKey":"abc","items":[{"token":"t"}]},"keep":"ok"}"#,
        4096,
    );
    assert!(!text.contains("hunter2"));
    assert!(!text.contains("abc"));
    assert!(!text.contains("\"t\""));
    assert!(text.contains("\"keep\":\"ok\""));
    assert_eq!(text.matches("[redacted]").count(), 3);

    assert_eq!(redact_payload(b"not json {", 4096), "not json {");
    assert_eq!(truncate("line\nbreak", 4), "line");
    assert_eq!(truncate("a\nb", 16), "a\\nb");
}

#[tokio::test]
async fn a_finished_request_logs_one_lavalink_shaped_line() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(subscriber(&capture));

    let mut state = app_state(Some("letmein"));
    state.sessions.reserve_for("session", 1);
    state.request_log = RequestLoggingConfig {
        include_client_info: true,
        include_headers: true,
        ..RequestLoggingConfig::default()
    };
    let address = spawn_server(state).await;

    let response = request_with_headers(
        address,
        "PATCH",
        "/v4/sessions/session?trace=true",
        Some("letmein"),
        Some(r#"{"timeout":1,"password":"hunter2"}"#),
        &[("X-Trace", "yes")],
    )
    .await;
    assert_eq!(response.status, 200);

    let text = capture.text();
    let line = text
        .lines()
        .find(|line| line.contains("PATCH /v4/sessions/session"))
        .unwrap_or_else(|| panic!("no request line in {text:?}"));
    assert!(line.contains("PATCH /v4/sessions/session?trace=true"), "{line}");
    assert!(line.contains(", client=127.0.0.1"), "{line}");
    assert!(line.contains(", headers={"), "{line}");
    assert!(line.contains("authorization=[redacted]"), "{line}");
    assert!(line.contains("x-trace=[yes]"), "{line}");
    assert!(line.contains(", payload="), "{line}");
    assert!(line.contains("\"password\":\"[redacted]\""), "{line}");
    assert!(!line.contains("hunter2"), "{line}");
    assert!(!line.contains("letmein"), "{line}");
    assert!(!line.contains("status="), "{line}");
    assert!(!line.contains("duration_ms="), "{line}");
}

#[tokio::test]
async fn a_disabled_request_log_writes_nothing() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(subscriber(&capture));

    let mut state = app_state(Some("letmein"));
    state.request_log.enabled = false;
    let address = spawn_server(state).await;
    let response =
        request_with_headers(address, "GET", "/v4/info", Some("letmein"), None, &[]).await;
    assert_eq!(response.status, 200);
    assert!(!capture.text().contains("/v4/info"));
}

#[tokio::test]
async fn before_request_adds_the_prefixed_line_without_a_payload() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(subscriber(&capture));

    let mut state = app_state(Some("letmein"));
    state.sessions.reserve_for("session", 1);
    state.request_log.before_request = true;
    let address = spawn_server(state).await;
    let response = request_with_headers(
        address,
        "PATCH",
        "/v4/sessions/session",
        Some("letmein"),
        Some(r#"{"timeout":1}"#),
        &[],
    )
    .await;
    assert_eq!(response.status, 200);

    let text = capture.text();
    let lines: Vec<&str> =
        text.lines().filter(|line| line.contains("PATCH /v4/sessions/session")).collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains(">> PATCH /v4/sessions/session"), "{}", lines[0]);
    assert!(!lines[0].contains(", payload="), "{}", lines[0]);
    assert!(!lines[1].contains(">> "), "{}", lines[1]);
    assert!(lines[1].contains(", payload={\"timeout\":1}"), "{}", lines[1]);
}

#[tokio::test]
async fn websocket_lifecycle_logs_identity_and_close_cause() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(subscriber(&capture));

    let state = app_state(Some("letmein"));
    let store = Arc::clone(&state.sessions);
    let app = crate::app::router(state);
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let _ =
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
    });

    let mut request =
        format!("ws://{address}/v4/websocket").into_client_request().expect("websocket request");
    request.headers_mut().insert("authorization", HeaderValue::from_static("letmein"));
    request.headers_mut().insert("user-id", HeaderValue::from_static("42"));
    request.headers_mut().insert("client-name", HeaderValue::from_static("test/1.0"));
    let (mut socket, _) = connect_async(request).await.expect("websocket handshake");
    assert!(socket.next().await.is_some(), "ready frame");

    socket.close(None).await.expect("close");
    for _ in 0..100 {
        if store.live() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(store.live(), 0);

    let text = capture.text();
    let opened = text
        .lines()
        .find(|line| line.contains("websocket session established"))
        .unwrap_or_else(|| panic!("no open line in {text:?}"));
    assert!(opened.contains("session_id="), "{opened}");
    assert!(opened.contains("user_id=42"), "{opened}");
    assert!(opened.contains("remote=127.0.0.1"), "{opened}");
    assert!(opened.contains("resumed=false"), "{opened}");
    assert!(opened.contains("client_name=\"test/1.0\""), "{opened}");

    let closed = text
        .lines()
        .find(|line| line.contains("websocket session closed"))
        .unwrap_or_else(|| panic!("no close line in {text:?}"));
    assert!(closed.contains("cause=\"peer\""), "{closed}");
    assert!(closed.contains("code=1005"), "{closed}");
    assert!(closed.contains("resumable=false"), "{closed}");
}

#[tokio::test]
async fn session_park_resume_and_expiry_are_logged() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(subscriber(&capture));

    let store = Arc::new(crate::session::SessionStore::default());
    store.reserve("parked");
    store.update("parked", Some(true), Some(0));
    store.close("parked");
    assert!(store.begin_resume("parked"));
    store.close("parked");

    for _ in 0..100 {
        if store.paused() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(store.paused(), 0);

    let text = capture.text();
    assert!(text.contains("session parked for resume"), "{text}");
    assert!(text.contains("session resumed"), "{text}");
    assert!(text.contains("resumable session expired"), "{text}");
}
