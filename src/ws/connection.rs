use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::sync::watch;
use tokio::time::{Instant, interval, interval_at};

use crate::app::AppState;
use crate::players::{active_player_update_messages, player_update_messages};
use crate::rest::middleware::auth::secret_eq;
use crate::session::{SessionSink, SessionStore, UpdateQueue};
use crate::stats;

/// Session-id alphabet: `a-z` and `0-9`.
const SESSION_ID_ALPHABET: [u8; 36] = {
    let mut pool = [0u8; 36];
    let mut i = 0;
    while i < 26 {
        pool[i] = b'a' + i as u8;
        i += 1;
    }
    while i < 36 {
        pool[i] = b'0' + (i - 26) as u8;
        i += 1;
    }
    pool
};
pub(super) const SESSION_ID_LEN: usize = 16;
const ACCEPT_BELOW: u8 = 36 * 7;
const MAX_ID_ATTEMPTS: u32 = 128;
const MAX_WRITE_BUFFER: usize = 1024 * 1024;
pub(crate) const MAX_INBOUND_FRAME: usize = 64 * 1024;
const MAX_INBOUND_MESSAGE: usize = 64 * 1024;
pub(super) const STATS_INTERVAL: Duration = Duration::from_secs(60);
const OUTBOUND_EVENTS: usize = 64;

pub(super) fn authorize_handshake(authorization: Option<&str>, password: Option<&str>) -> bool {
    match (authorization, password) {
        (None, None) => true,
        (Some(given), Some(expected)) => secret_eq(given.as_bytes(), expected.as_bytes()),
        _ => false,
    }
}

#[cfg(test)]
pub(super) fn is_valid_user_id(user_id: Option<&str>) -> bool {
    parse_user_id(user_id).is_some()
}

fn parse_user_id(user_id: Option<&str>) -> Option<u64> {
    let user_id = user_id?.parse::<i64>().ok()?;
    u64::try_from(user_id).ok().filter(|user_id| *user_id != 0)
}

pub(super) fn generate_session_id() -> Option<String> {
    let mut id = String::with_capacity(SESSION_ID_LEN);
    let mut buf = [0u8; 16];
    while id.len() < SESSION_ID_LEN {
        getrandom::fill(&mut buf).ok()?;
        for &byte in &buf {
            if byte < ACCEPT_BELOW {
                id.push(SESSION_ID_ALPHABET[usize::from(byte % 36)] as char);
                if id.len() == SESSION_ID_LEN {
                    break;
                }
            }
        }
    }
    Some(id)
}

fn reserve_unique_id(store: &SessionStore, user_id: u64) -> Option<String> {
    for _ in 0..MAX_ID_ATTEMPTS {
        let candidate = generate_session_id()?;
        if store.reserve_for(&candidate, user_id) {
            return Some(candidate);
        }
    }
    None
}

fn header<'h>(headers: &'h HeaderMap, name: &'static str) -> Option<&'h str> {
    headers.get(HeaderName::from_static(name)).and_then(|value| value.to_str().ok())
}

pub(super) fn ready_frame(session_id: &str, resumed: bool) -> Utf8Bytes {
    Utf8Bytes::from(format!(
        "{{\"op\":\"ready\",\"resumed\":{resumed},\"sessionId\":\"{session_id}\"}}"
    ))
}

pub(crate) async fn websocket(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
) -> Response {
    if !authorize_handshake(header(&headers, "authorization"), state.password()) {
        tracing::warn!(remote = %remote.ip(), "websocket authorization failed");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(user_id) = parse_user_id(header(&headers, "user-id")) else {
        tracing::warn!(
            remote = %remote.ip(),
            "websocket connection attempt with a missing or invalid User-Id"
        );
        return StatusCode::BAD_REQUEST.into_response();
    };

    let client_name = header(&headers, "client-name").map(str::to_owned);
    let user_agent = header(&headers, "user-agent").map(str::to_owned);

    let (session_id, resumed) = match header(&headers, "session-id") {
        Some(id) if state.sessions.begin_resume(id) => (id.to_owned(), true),
        _ => match reserve_unique_id(&state.sessions, user_id) {
            Some(id) => (id, false),
            None => {
                tracing::error!(
                    remote = %remote.ip(),
                    "could not allocate a websocket session id, rejecting handshake"
                );
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        },
    };

    let mut response = ws
        .max_write_buffer_size(MAX_WRITE_BUFFER)
        .max_frame_size(MAX_INBOUND_FRAME)
        .max_message_size(MAX_INBOUND_MESSAGE)
        .on_upgrade(move |socket| {
            handle_connection(
                socket,
                state,
                session_id,
                user_id,
                remote.ip(),
                resumed,
                client_name,
                user_agent,
            )
        });
    response.headers_mut().insert(
        HeaderName::from_static("session-resumed"),
        HeaderValue::from_static(if resumed { "true" } else { "false" }),
    );
    response
}

#[derive(Clone, Copy)]
enum Closing {
    Peer,
    Shutdown,
    SendFailed,
    StreamEnded,
    LifecycleUndelivered,
}

impl Closing {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Peer => "peer",
            Self::Shutdown => "shutdown",
            Self::SendFailed => "send failed",
            Self::StreamEnded => "stream ended",
            Self::LifecycleUndelivered => "lifecycle backlog",
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    mut socket: WebSocket,
    state: AppState,
    session_id: String,
    user_id: u64,
    remote: IpAddr,
    resumed: bool,
    client_name: Option<String>,
    user_agent: Option<String>,
) {
    log_established(
        &session_id,
        user_id,
        remote,
        resumed,
        client_name.as_deref(),
        user_agent.as_deref(),
    );

    if socket.send(Message::Text(ready_frame(&session_id, resumed))).await.is_err() {
        state.sessions.close(&session_id).shutdown().await;
        tracing::debug!(session_id = %session_id, "websocket closed before the ready frame was sent");
        return;
    }

    let mut ticker = interval(STATS_INTERVAL);
    let (messages, mut outbound_events) = tokio::sync::mpsc::channel(OUTBOUND_EVENTS);
    let updates = Arc::new(UpdateQueue::default());
    let (cancel, mut cancel_events) = watch::channel(None);
    let sink = SessionSink { messages, updates: Arc::clone(&updates), cancel };
    if !state.sessions.set_outbound(&session_id, sink) {
        state.sessions.close(&session_id).shutdown().await;
        return;
    }
    if resumed
        && let Some(players) = state.sessions.players(&session_id)
        && !send_text_messages(&mut socket, player_update_messages(&players).await).await
    {
        state.sessions.close(&session_id).shutdown().await;
        return;
    }
    let update_interval = state.player_update_interval();
    let mut player_updates = interval_at(Instant::now() + update_interval, update_interval);

    let mut stopping = state.shutdown.subscribe();
    let mut peer_close: Option<(u16, String)> = None;
    let closing = 'session: loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Some(text) = stats::ws_stats(&state, &session_id).await
                    && socket.send(Message::Text(Utf8Bytes::from(text))).await.is_err()
                {
                    break Closing::SendFailed;
                }
            }
            _ = player_updates.tick() => {
                let Some(players) = state.sessions.players(&session_id) else { break Closing::StreamEnded };
                if !send_text_messages(
                    &mut socket,
                    active_player_update_messages(&players).await,
                )
                .await
                {
                    break Closing::SendFailed;
                }
            }
            _ = stopping.changed() => {
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: 1001,
                        reason: Utf8Bytes::default(),
                    })))
                    .await;
                break Closing::Shutdown;
            }
            message = outbound_events.recv() => {
                let Some(message) = message else { break Closing::StreamEnded };
                if socket.send(Message::Text(Utf8Bytes::from(message))).await.is_err() {
                    break Closing::SendFailed;
                }
            }
            _ = updates.notified() => {
                for message in updates.drain() {
                    if socket.send(Message::Text(Utf8Bytes::from(message))).await.is_err() {
                        break 'session Closing::SendFailed;
                    }
                }
            }
            result = cancel_events.changed() => {
                if result.is_err() {
                    break Closing::StreamEnded;
                }
                let reason = cancel_events.borrow_and_update().clone().unwrap_or_default();
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: 1011,
                        reason: Utf8Bytes::from(reason),
                    })))
                    .await;
                break Closing::LifecycleUndelivered;
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(_))) => {
                        tracing::warn!(
                            session_id = %session_id,
                            "Lavalink v4 does not support websocket messages. Please use the REST api."
                        );
                    }
                    Some(Ok(Message::Binary(_))) => {
                        tracing::trace!(
                            session_id = %session_id,
                            "ignoring an inbound binary websocket message"
                        );
                    }
                    Some(Ok(Message::Close(frame))) => {
                        peer_close = frame.map(|frame| (frame.code, frame.reason.to_string()));
                        break Closing::Peer;
                    }
                    None => break Closing::Peer,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break Closing::SendFailed;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Err(_)) => break Closing::Peer,
                }
            }
        }
    };

    let resumable = state.sessions.update(&session_id, None, None).map(|(resuming, _)| resuming);
    let closed = state.sessions.close(&session_id);
    let code = match closing {
        Closing::Shutdown => 1001,
        Closing::LifecycleUndelivered => 1011,
        _ => peer_close.as_ref().map_or(1005, |(code, _)| *code),
    };
    let reason = match closing {
        Closing::LifecycleUndelivered => cancel_events.borrow().clone().unwrap_or_default(),
        _ => peer_close.as_ref().map_or_else(String::new, |(_, reason)| reason.clone()),
    };
    tracing::info!(
        session_id = %session_id,
        user_id,
        remote = %remote,
        resumed,
        resumable = resumable.unwrap_or(false),
        cause = closing.as_str(),
        code,
        reason = %reason,
        "websocket session closed"
    );
    closed.shutdown().await;
}

async fn send_text_messages(socket: &mut WebSocket, messages: Vec<String>) -> bool {
    for message in messages {
        if socket.send(Message::Text(Utf8Bytes::from(message))).await.is_err() {
            return false;
        }
    }
    true
}

fn log_established(
    session_id: &str,
    user_id: u64,
    remote: IpAddr,
    resumed: bool,
    client_name: Option<&str>,
    user_agent: Option<&str>,
) {
    tracing::info!(
        session_id,
        user_id,
        remote = %remote,
        resumed,
        client_name = client_name.unwrap_or(""),
        "websocket session established"
    );
    if client_name.is_none() {
        match user_agent {
            Some(agent) => tracing::warn!(
                session_id,
                remote = %remote,
                %agent,
                "library developers: specify a `Client-Name` header"
            ),
            None => tracing::warn!(
                session_id,
                remote = %remote,
                "library developers: specify a `Client-Name` header"
            ),
        }
    }
}

#[cfg(test)]
#[path = "../tests/client_name_log.rs"]
mod tests;
