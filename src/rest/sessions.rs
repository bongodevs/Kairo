//! Session configuration endpoint (`PATCH /v4/sessions/{sessionId}`).

use axum::extract::{Path, State};
use axum::Json;

use crate::node::AppState;
use crate::protocol::omissible::Omissible;
use crate::protocol::session::{Session, SessionUpdate};
use crate::rest::error::{RestError, RestResult};
use crate::rest::json::JsonBody;

/// `PATCH /v4/sessions/{sessionId}`, turn resuming on or off and set the resume timeout.
pub async fn patch_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    JsonBody(update): JsonBody<SessionUpdate>,
) -> RestResult<Json<Session>> {
    let context = state
        .sockets()
        .get_any(&session_id)
        .ok_or_else(|| RestError::not_found(format!("Session {session_id} not found")))?;

    if let Omissible::Present(resuming) = update.resuming {
        context.set_resuming(resuming);
    }
    if let Omissible::Present(timeout) = update.timeout {
        context.set_resume_timeout_secs(timeout.max(0) as u64);
    }

    Ok(Json(Session {
        resuming: context.is_resuming(),
        timeout: context.resume_timeout_secs() as i64,
    }))
}
