//! Read, replace and clear a guild's SponsorBlock skip categories.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use crate::node::AppState;
use crate::rest::error::{RestError, RestResult};
use crate::rest::json::JsonBody;
use crate::session::SocketContext;

fn session(state: &AppState, session_id: &str) -> RestResult<Arc<SocketContext>> {
    state
        .sockets()
        .get_any(session_id)
        .ok_or_else(|| RestError::not_found(format!("Session {session_id} not found")))
}

/// `GET /v4/sessions/{sessionId}/players/{guildId}/sponsorblock/categories`
pub async fn get_categories(
    State(state): State<AppState>,
    Path((session_id, guild_id)): Path<(String, u64)>,
) -> RestResult<Json<Vec<String>>> {
    let context = session(&state, &session_id)?;
    let categories = context
        .get_sponsorblock_categories(guild_id)
        .ok_or_else(|| RestError::not_found("No SponsorBlock categories set for this guild"))?;
    Ok(Json(categories.into_iter().collect()))
}

/// `PUT /v4/sessions/{sessionId}/players/{guildId}/sponsorblock/categories`
pub async fn set_categories(
    State(state): State<AppState>,
    Path((session_id, guild_id)): Path<(String, u64)>,
    JsonBody(categories): JsonBody<Vec<String>>,
) -> RestResult<StatusCode> {
    let context = session(&state, &session_id)?;
    context.set_sponsorblock_categories(guild_id, categories.into_iter().collect::<HashSet<_>>());
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v4/sessions/{sessionId}/players/{guildId}/sponsorblock/categories`
pub async fn delete_categories(
    State(state): State<AppState>,
    Path((session_id, guild_id)): Path<(String, u64)>,
) -> RestResult<StatusCode> {
    let context = session(&state, &session_id)?;
    context.remove_sponsorblock_categories(guild_id);
    Ok(StatusCode::NO_CONTENT)
}
