use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::session::SessionStore;
use crate::test_support::app_state;
async fn spawn(password: Option<&str>) -> (SocketAddr, Arc<SessionStore>) {
    let state = app_state(password);
    let store = Arc::clone(&state.sessions);
    let app = crate::app::router(state);
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ =
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
    });
    (addr, store)
}

/// Issue a PATCH and return `(status, body)`. Written over a raw socket to avoid an HTTP client
/// dependency in the test tree.
async fn patch(addr: SocketAddr, path: &str, body: &str, auth: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "PATCH {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status =
        response.split_whitespace().nth(1).and_then(|code| code.parse::<u16>().ok()).unwrap_or(0);
    let body =
        response.split_once("\r\n\r\n").map_or_else(String::new, |(_, rest)| rest.to_owned());
    (status, body)
}

#[test]
fn update_sets_each_field_and_returns_the_result() {
    let store = SessionStore::default();
    store.reserve("s1");
    // Neither field present: unchanged, defaults reported (resuming off, 60 s window).
    assert_eq!(store.update("s1", None, None), Some((false, 60)));
    // Only resuming on: timeout stays at its default.
    assert_eq!(store.update("s1", Some(true), None), Some((true, 60)));
    // Only timeout: resuming stays on.
    assert_eq!(store.update("s1", None, Some(30)), Some((true, 30)));
    // A negative window is reported as sent; only the timer clamps it.
    assert_eq!(store.update("s1", None, Some(-1)), Some((true, -1)));
}

#[test]
fn update_of_an_unknown_id_is_none() {
    let store = SessionStore::default();
    assert_eq!(store.update("absent", Some(true), Some(5)), None);
}

#[tokio::test]
async fn patch_enables_resume_on_a_live_session() {
    let (addr, store) = spawn(Some("letmein")).await;
    store.reserve("client1234567890");
    let (status, body) = patch(
        addr,
        "/v4/sessions/client1234567890",
        "{\"resuming\":true,\"timeout\":10}",
        "letmein",
    )
    .await;
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["resuming"], true);
    assert_eq!(json["timeout"], 10);
}

#[tokio::test]
async fn patch_on_an_unknown_session_is_not_found() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let (status, _body) =
        patch(addr, "/v4/sessions/does-not-exist", "{\"resuming\":true}", "letmein").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn patch_requires_authorization() {
    let (addr, store) = spawn(Some("letmein")).await;
    store.reserve("client1234567890");
    let (status, _body) = patch(addr, "/v4/sessions/client1234567890", "{}", "wrong").await;
    // Wrong token on a REST route is 403 (present-but-incorrect), before the handler runs.
    assert_eq!(status, 403);
}

#[tokio::test]
async fn malformed_body_is_bad_request() {
    let (addr, store) = spawn(Some("letmein")).await;
    store.reserve("client1234567890");
    let (status, _body) =
        patch(addr, "/v4/sessions/client1234567890", "{not json}", "letmein").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn a_negative_resume_timeout_is_accepted() {
    let (addr, store) = spawn(Some("letmein")).await;
    store.reserve("client1234567890");
    let (status, body) =
        patch(addr, "/v4/sessions/client1234567890", r#"{"timeout":-1}"#, "letmein").await;
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["timeout"], -1);
}

#[tokio::test]
async fn a_negative_timeout_parks_the_session_with_no_window() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    store.update("s1", Some(true), Some(-1));
    store.close("s1");
    let expired = tokio::time::timeout(Duration::from_secs(1), async {
        while store.is_paused("s1") {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(expired.is_ok(), "a negative resume window must expire the session on close");
}
