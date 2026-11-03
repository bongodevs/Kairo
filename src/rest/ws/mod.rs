//! The `/v4/websocket` endpoint: the handshake and the per-session read/write halves.
//!
//! A client opens one socket per session and receives every event on it. The socket carries no
//! inbound commands, those go over REST; a session id in the handshake asks to resume an earlier one.

mod sender;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use tokio::sync::mpsc;

use crate::node::AppState;
use crate::protocol::message::Message;
use crate::rest::error::RestError;
use crate::session::{SocketContext, OUTBOUND_CAPACITY};
use crate::utils::locked;

/// The v3 websocket probes answer `200` with an upgrade warning instead of `404`, so an old client
/// library fails with a readable message. Authentication still applies, as it does in the reference.
pub async fn legacy_websocket() -> &'static str {
    tracing::warn!(
        "This is the old Lavalink websocket endpoint. Please use /v4/websocket instead. If you are \
         using a client library, update it to a Lavalink v4 compatible version or use Lavalink v3."
    );
    "This is the old Lavalink websocket endpoint. Please use /v4/websocket instead."
}

/// `GET /v4/websocket`, upgrade the connection once the handshake headers check out.
pub async fn websocket_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let user_id = headers
        .get("user-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|user_id| *user_id != 0);
    let Some(user_id) = user_id else {
        return RestError::bad_request("Missing or invalid User-Id header").into_response();
    };

    let session_id = header_string(&headers, "session-id");
    let client_name = header_string(&headers, "client-name");
    let user_agent = header_string(&headers, "user-agent");

    // `Session-Resumed` rides on the 101 response, so claim the parked session before the upgrade.
    // The claim is the single decision behind both the header and `ready.resumed`, and it keeps two
    // handshakes with the same Session-Id from both resuming the entry.
    let claimed = session_id
        .as_deref()
        .and_then(|id| claim_parked(&state, id));
    // Only a claim or a live session resumes. A parked entry already claimed by another handshake
    // must not make this one advertise a resume it will not get.
    let resumable = claimed.is_some()
        || session_id
            .as_deref()
            .is_some_and(|id| state.sockets().get(id).is_some());

    // The claim is handed to whichever callback runs: the socket task on success, the re-park path on
    // a failed upgrade, so a transport error cannot strand the session in the active map.
    let claim = Arc::new(Mutex::new(claimed));
    let claim_on_upgrade = Arc::clone(&claim);
    let claim_on_failure = Arc::clone(&claim);
    let failure_state = state.clone();

    let mut response = ws
        .on_failed_upgrade(move |error| {
            tracing::warn!(%error, "websocket upgrade failed");
            if let Some(context) = locked(&claim_on_failure).take() {
                release_claim(&failure_state, context);
            }
        })
        .on_upgrade(move |socket| {
            let claimed = locked(&claim_on_upgrade).take();
            handle_socket(
                socket,
                state,
                user_id,
                session_id,
                claimed,
                client_name,
                user_agent,
            )
        });
    response.headers_mut().insert(
        "Session-Resumed",
        if resumable {
            HeaderValue::from_static("true")
        } else {
            HeaderValue::from_static("false")
        },
    );
    response
}

// Claim a parked session for this handshake before the upgrade, cancelling its expiry timer so the
// socket's own resume path re-arms it. `None` means this handshake creates a fresh session.
fn claim_parked(state: &AppState, session_id: &str) -> Option<Arc<SocketContext>> {
    let context = state.sockets().claim_resumable(session_id)?;
    context.stop_resume_timeout();
    Some(context)
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    user_id: u64,
    requested_session: Option<String>,
    claimed: Option<Arc<SocketContext>>,
    client_name: Option<String>,
    user_agent: Option<String>,
) {
    let (sender, receiver) = mpsc::channel::<Message>(OUTBOUND_CAPACITY);

    let (context, resumed, epoch, backlog) = attach(
        &state,
        requested_session.as_deref(),
        claimed,
        user_id,
        sender,
    );
    tracing::info!(session = %context.session_id, resumed, "websocket ready");
    if !resumed {
        log_new_connection(client_name, user_agent);
    }

    let session_id = context.session_id.clone();
    let reason = sender::pump(socket, receiver, backlog, session_id).await;

    tracing::debug!(
        session = %context.session_id,
        ?reason,
        "websocket closing"
    );
    on_disconnect(&state, context, epoch);
}

fn log_new_connection(client_name: Option<String>, user_agent: Option<String>) {
    match (client_name, user_agent) {
        (Some(name), _) => tracing::info!(client = %name, "Connection successfully established"),
        (None, agent) => {
            tracing::info!("Connection successfully established");
            match agent {
                Some(agent) => tracing::warn!(
                    "Library developers: Please specify a 'Client-Name' header. User agent: {agent}"
                ),
                None => {
                    tracing::warn!("Library developers: Please specify a 'Client-Name' header.")
                }
            }
        }
    }
}

/// Which half ended first, for the teardown log.
#[derive(Debug)]
pub(super) enum Ended {
    Read,
    Write,
}

fn attach(
    state: &AppState,
    requested_session: Option<&str>,
    claimed: Option<Arc<SocketContext>>,
    user_id: u64,
    sender: mpsc::Sender<Message>,
) -> (Arc<SocketContext>, bool, u64, Vec<Message>) {
    if let Some(context) = claimed {
        state.sockets().finish_claim(&context.session_id);
        state.sockets().insert(Arc::clone(&context));
        let (epoch, queued) = context.resume_with(sender);
        let mut backlog = Vec::with_capacity(queued.len() + 1);
        backlog.push(ready(&context.session_id, true));
        backlog.extend(queued);
        for player in context.players() {
            player.send_player_update();
        }
        return (context, true, epoch, backlog);
    }
    if let Some(id) = requested_session {
        if let Some(context) = state.sockets().get(id) {
            let epoch = context.attach_sender(sender);
            for player in context.players() {
                player.send_player_update();
            }
            tracing::info!(session = %context.session_id, "reattached to a live session");
            let backlog = vec![ready(&context.session_id, true)];
            return (context, true, epoch, backlog);
        }
    }

    let session_id = state.sockets().generate_session_id();
    let context = SocketContext::new(
        session_id.clone(),
        user_id,
        state.manager().clone(),
        state.config().crossfade.to_engine(),
        state.lyrics_service().cloned(),
        sender,
    );
    state.sockets().insert(Arc::clone(&context));
    state.arm_session_stats(&context);
    let epoch = context.connection_epoch();
    (context, false, epoch, vec![ready(&session_id, false)])
}

fn ready(session_id: &str, resumed: bool) -> Message {
    Message::Ready {
        resumed,
        session_id: session_id.to_string(),
    }
}

fn on_disconnect(state: &AppState, context: Arc<SocketContext>, epoch: u64) {
    let session_id = context.session_id.clone();

    // A client that reconnects before its old socket finishes closing bumps the epoch; without this
    // guard the late close would park or destroy the session the new socket is already using.
    if context.connection_epoch() != epoch {
        tracing::debug!(session = %session_id, "stale socket closed; session already reattached");
        return;
    }

    if context.is_resuming() {
        park_session(state, context);
    } else {
        state.sockets().remove(&session_id);
        context.shutdown();
        tracing::info!(session = %session_id, "session closed");
    }
}

// Move a session to the resumable map and arm its expiry timer after a resumable disconnect.
fn park_session(state: &AppState, context: Arc<SocketContext>) {
    let session_id = context.session_id.clone();
    let timeout = context.resume_timeout_secs();
    state.sockets().move_to_resumable(&session_id);
    context.pause();
    tracing::info!(session = %session_id, timeout, "session parked for resume");
    arm_resume_timer(state, &context);
}

// A failed upgrade leaves the claimed session where it already was, parked, with a fresh window.
fn release_claim(state: &AppState, context: Arc<SocketContext>) {
    let session_id = context.session_id.clone();
    let timeout = context.resume_timeout_secs();
    state.sockets().release_claim(&session_id);
    tracing::info!(session = %session_id, timeout, "upgrade failed; session stays parked");
    arm_resume_timer(state, &context);
}

fn arm_resume_timer(state: &AppState, context: &Arc<SocketContext>) {
    let session_id = context.session_id.clone();
    let timeout = context.resume_timeout_secs();
    let state = state.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(timeout)).await;
        if let Some(context) = state.sockets().drop_resumable(&session_id) {
            tracing::info!(session = %session_id, "resume window expired; destroying session");
            context.shutdown();
        }
    });
    context.arm_resume_timeout(timer.abort_handle());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn park(state: &AppState, id: &str) {
        let (sender, _receiver) = mpsc::channel(OUTBOUND_CAPACITY);
        let context = SocketContext::new(
            id.to_string(),
            1,
            player::AudioPlayerManager::new(),
            None,
            None,
            sender,
        );
        state.sockets().insert(context);
        state.sockets().move_to_resumable(id);
    }

    // The header is written from the claim, and the attach path consumes that same claim, so a
    // parked session cannot advertise a resume and then start a fresh one.
    #[test]
    fn the_resume_header_and_ready_come_from_one_claim() {
        let state = AppState::test_state();
        park(&state, "parked");

        let claimed = claim_parked(&state, "parked");
        assert!(claimed.is_some(), "the handshake claims the parked session");

        let (sender, _receiver) = mpsc::channel(OUTBOUND_CAPACITY);
        let (context, resumed, _, backlog) = attach(&state, Some("parked"), claimed, 1, sender);

        assert!(resumed);
        assert_eq!(context.session_id, "parked");
        assert!(matches!(
            backlog.first(),
            Some(Message::Ready { resumed: true, .. })
        ));
    }

    // Two handshakes may both see the parked id; only the one that claims it resumes, the other
    // creates a fresh session instead of attaching to the same context.
    #[tokio::test]
    async fn only_one_handshake_can_claim_a_parked_session() {
        let state = AppState::test_state();
        park(&state, "parked");

        assert!(claim_parked(&state, "parked").is_some());
        assert!(claim_parked(&state, "parked").is_none());
        assert!(state.sockets().get("parked").is_none());

        let (sender, _receiver) = mpsc::channel(OUTBOUND_CAPACITY);
        let (context, resumed, _, _) = attach(&state, Some("parked"), None, 1, sender);

        assert!(!resumed);
        assert_ne!(context.session_id, "parked");
        // The winner's session stays parked and reachable while its upgrade completes.
        assert!(state.sockets().get_any("parked").is_some());
    }

    // A transport-level upgrade failure must put the claim back so the client can retry.
    #[tokio::test]
    async fn a_failed_upgrade_releases_the_claim() {
        let state = AppState::test_state();
        park(&state, "parked");

        let claimed = claim_parked(&state, "parked").expect("first claim");
        release_claim(&state, claimed);

        assert!(claim_parked(&state, "parked").is_some());
    }
}
