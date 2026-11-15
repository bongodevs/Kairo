use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;
use crate::players::{GuildId, player_response};
use crate::rest::extract::ApiPath;

pub(crate) use crate::players::patch_player;

pub(crate) async fn get_players(
    State(state): State<AppState>,
    ApiPath(session_id): ApiPath<String>,
) -> Response {
    let Some(players) = state.sessions.players(&session_id) else {
        return (StatusCode::NOT_FOUND, "Session not found").into_response();
    };
    let mut response = Vec::new();
    for player in players.list() {
        match player_response(&player).await {
            Ok(player) => response.push(player),
            Err(status) => return status.into_response(),
        }
    }
    Json(response).into_response()
}

pub(crate) async fn get_player(
    State(state): State<AppState>,
    ApiPath((session_id, guild_id)): ApiPath<(String, u64)>,
) -> Response {
    let Some(players) = state.sessions.players(&session_id) else {
        return (StatusCode::NOT_FOUND, "Session not found").into_response();
    };
    let Some(player) = players.get(GuildId::new(guild_id)) else {
        return (StatusCode::NOT_FOUND, "Player not found").into_response();
    };
    match player_response(&player).await {
        Ok(player) => Json(player).into_response(),
        Err(status) => status.into_response(),
    }
}

pub(crate) async fn delete_player(
    State(state): State<AppState>,
    ApiPath((session_id, guild_id)): ApiPath<(String, u64)>,
) -> Response {
    if state.sessions.players(&session_id).is_none() {
        return (StatusCode::NOT_FOUND, "Session not found").into_response();
    }
    if let Some(player) =
        state.sessions.remove_player(&session_id, GuildId::new(guild_id)).flatten()
    {
        let _ = player.shutdown().await;
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
#[path = "../../tests/player_routes.rs"]
mod tests;
