use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::rest::extract::{ApiJson, ApiPath};

#[derive(Debug, Deserialize)]
pub(crate) struct SessionUpdate {
    #[serde(default)]
    resuming: Option<bool>,
    #[serde(default)]
    timeout: Option<i64>,
}

#[derive(Debug, Serialize)]
struct Session {
    resuming: bool,
    timeout: i64,
}

pub(crate) async fn patch_session(
    State(state): State<AppState>,
    ApiPath(session_id): ApiPath<String>,
    ApiJson(update): ApiJson<SessionUpdate>,
) -> Response {
    match state.sessions.update(&session_id, update.resuming, update.timeout) {
        Some((resuming, timeout)) => Json(Session { resuming, timeout }).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
#[path = "../../tests/session.rs"]
mod tests;
