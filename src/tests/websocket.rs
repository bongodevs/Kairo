//! Socket-level tests: the real router on an ephemeral port, driven by a tungstenite client.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as ClientRequest;
use tokio_tungstenite::tungstenite::http::{
    HeaderName, HeaderValue, StatusCode as HttpStatus, header::AUTHORIZATION,
};

use super::*;
use crate::app::AppState;
use crate::session::SessionStore;
use crate::test_support::app_state;
#[path = "websocket_handshake.rs"]
mod handshake;
#[path = "websocket_player.rs"]
mod player_sequence;
#[path = "websocket_traffic.rs"]
mod traffic;

/// A frame we wait for must arrive well inside the test timeout; five seconds is far past any
/// localhost round-trip and far before a hung test becomes useless.
const FRAME_WAIT: Duration = Duration::from_secs(5);

fn state(password: Option<&str>) -> AppState {
    let mut state = app_state(password);
    state.player_update_interval = Duration::from_millis(100);
    state
}

/// Spin up the real router on an ephemeral port; return the address and the session store so a
/// test can assert on the live-session count.
async fn spawn(password: Option<&str>) -> (SocketAddr, Arc<SessionStore>) {
    let (addr, store, _shutdown) = spawn_notified(password).await;
    (addr, store)
}

/// As `spawn`, but also hands back the shutdown notice: flipping it is what a stop signal does.
async fn spawn_notified(
    password: Option<&str>,
) -> (SocketAddr, Arc<SessionStore>, tokio::sync::watch::Sender<bool>) {
    let state = state(password);
    let store = Arc::clone(&state.sessions);
    let shutdown = state.shutdown.clone();
    let app = crate::app::router(state);
    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ =
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
    });
    (addr, store, shutdown)
}

fn request(addr: SocketAddr, authorization: Option<&str>, user_id: Option<&str>) -> ClientRequest {
    let mut req = format!("ws://{addr}/v4/websocket").into_client_request().unwrap();
    let headers = req.headers_mut();
    if let Some(value) = authorization {
        headers.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
    }
    if let Some(value) = user_id {
        headers.insert(HeaderName::from_static("user-id"), HeaderValue::from_str(value).unwrap());
    }
    req
}

fn rejected_status(err: WsError) -> HttpStatus {
    match err {
        WsError::Http(response) => {
            assert_eq!(
                response
                    .headers()
                    .get("lavalink-api-version")
                    .and_then(|value| value.to_str().ok()),
                Some("4")
            );
            response.status()
        }
        other => unreachable!("expected an HTTP handshake rejection, got {other:?}"),
    }
}

/// Connect as an authorized client with a valid user id.
async fn connect(
    addr: SocketAddr,
) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    connect_async(request(addr, Some("letmein"), Some("170939974227541168")))
        .await
        .expect("handshake")
        .0
}

/// The next frame, parsed as JSON, bounded so a missing frame fails instead of hanging.
async fn next_json<T>(stream: &mut WebSocketStream<T>) -> serde_json::Value
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(FRAME_WAIT, stream.next())
        .await
        .expect("a frame in time")
        .expect("open")
        .expect("no transport error");
    serde_json::from_str(frame.into_text().expect("utf-8 text").as_str()).expect("json")
}
