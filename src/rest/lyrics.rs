//! The lyrics endpoints:
//!
//! - `GET /v4/lyrics?track=<encoded>&skipTrackSource=<bool>` for an encoded track.
//! - `GET /v4/sessions/{s}/players/{g}/track/lyrics?skipTrackSource=<bool>` for the current track.
//! - `POST /v4/sessions/{s}/players/{g}/lyrics/subscribe?skipTrackSource=<bool>` to subscribe.
//! - `DELETE /v4/sessions/{s}/players/{g}/lyrics/subscribe` to unsubscribe.
//!
//! These handlers decode inputs, run the provider fan-out for the two GETs, and toggle the
//! subscription; the live-lyrics machinery itself is in [`crate::session::lyrics`]. Every endpoint
//! needs the feature enabled, which [`AppState::lyrics_service`] reports.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use lyrics::{LyricsQuery, LyricsService};

use crate::node::AppState;
use crate::protocol::lyrics::Lyrics;
use crate::rest::error::{RestError, RestResult};
use crate::session::lyrics::query_from_info;
use crate::session::SocketContext;

// `503` when the feature is disabled.
fn service(state: &AppState) -> RestResult<Arc<LyricsService>> {
    state
        .lyrics_service()
        .cloned()
        .ok_or_else(|| RestError::new(StatusCode::SERVICE_UNAVAILABLE, "Lyrics are not enabled"))
}

fn session(state: &AppState, session_id: &str) -> RestResult<Arc<SocketContext>> {
    state
        .sockets()
        .get_any(session_id)
        .ok_or_else(|| RestError::not_found(format!("Session {session_id} not found")))
}

fn skip_track_source(params: &HashMap<String, String>) -> bool {
    params
        .get("skipTrackSource")
        .map(|v| v == "true")
        .unwrap_or(false)
}

// `200` with the [`Lyrics`] body, or `204 No Content` when no provider had anything.
async fn respond_with_lyrics(
    service: &LyricsService,
    query: &LyricsQuery,
    skip_track_source: bool,
) -> Response {
    let result = if skip_track_source {
        service.load_lyrics_skip_source(query).await
    } else {
        service.load_lyrics(query).await
    };
    match result {
        Some(data) => Json(Lyrics::from_data(&data)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// `GET /v4/lyrics`, lyrics for a base64-encoded track.
pub async fn get_lyrics(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> RestResult<Response> {
    let service = service(&state)?;
    let encoded = params
        .get("track")
        .ok_or_else(|| RestError::bad_request("Missing required query parameter 'track'"))?;

    let track = state
        .manager()
        .decode_track(encoded)
        .map_err(|err| RestError::from_friendly(&err))?
        .ok_or_else(|| RestError::bad_request("Could not decode the provided track"))?;

    let info = track.info();
    let query = LyricsQuery {
        title: info.title.clone(),
        author: info.author.clone(),
        identifier: info.identifier.clone(),
        source_name: track.source_name().to_string(),
        uri: info.uri.clone(),
        // `u64::MAX` is the engine's unknown-length sentinel for a live stream, which is no duration
        // a provider can match on.
        duration_ms: (info.length > 0 && info.length < u64::MAX).then_some(info.length),
        isrc: info.isrc.clone(),
    };

    Ok(respond_with_lyrics(&service, &query, skip_track_source(&params)).await)
}

/// `GET /v4/sessions/{sessionId}/players/{guildId}/track/lyrics`, the current track's lyrics.
pub async fn get_player_lyrics(
    State(state): State<AppState>,
    Path((session_id, guild_id)): Path<(String, u64)>,
    Query(params): Query<HashMap<String, String>>,
) -> RestResult<Response> {
    let service = service(&state)?;
    let context = session(&state, &session_id)?;
    let player = context
        .get_player(guild_id)
        .ok_or_else(|| RestError::not_found("Player not found"))?;

    let track = player
        .current_track()
        .ok_or_else(|| RestError::not_found("No track is currently playing"))?;

    let query = query_from_info(&track.info);
    Ok(respond_with_lyrics(&service, &query, skip_track_source(&params)).await)
}

/// `POST /v4/sessions/{sessionId}/players/{guildId}/lyrics/subscribe`, subscribe to live lyrics.
pub async fn subscribe(
    State(state): State<AppState>,
    Path((session_id, guild_id)): Path<(String, u64)>,
    Query(params): Query<HashMap<String, String>>,
) -> RestResult<StatusCode> {
    // Report the feature off before touching the session.
    service(&state)?;
    let context = session(&state, &session_id)?;
    let player = context.get_or_create_player(guild_id);
    player.subscribe_lyrics(skip_track_source(&params));
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v4/sessions/{sessionId}/players/{guildId}/lyrics/subscribe`, unsubscribe.
pub async fn unsubscribe(
    State(state): State<AppState>,
    Path((session_id, guild_id)): Path<(String, u64)>,
) -> RestResult<StatusCode> {
    service(&state)?;
    let context = session(&state, &session_id)?;
    if let Some(player) = context.get_player(guild_id) {
        player.unsubscribe_lyrics();
    }
    Ok(StatusCode::NO_CONTENT)
}
