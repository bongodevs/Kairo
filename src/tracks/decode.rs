use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use player::track::codec::{self, DecodeTrack, TrackCodecs};

use super::dto::{TrackResponse, to_response};
use crate::app::AppState;
use crate::rest::extract::{ApiJson, ApiQuery};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecodeQuery {
    #[serde(default)]
    encoded_track: Option<String>,
    #[serde(default)]
    track: Option<String>,
}

fn decode_one(encoded: &str, codecs: &TrackCodecs) -> Result<TrackResponse, StatusCode> {
    match codec::decode(encoded, codecs) {
        Ok(DecodeTrack::Track(track)) => {
            let snapshot = track.snapshot(track.initial_position());
            to_response(encoded, &snapshot).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        }
        Ok(DecodeTrack::UnsupportedVersion { .. }) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Ok(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(crate) async fn decodetrack(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<DecodeQuery>,
) -> Response {
    let Some(encoded) = query.encoded_track.or(query.track) else {
        return (StatusCode::BAD_REQUEST, "No track to decode provided").into_response();
    };
    match decode_one(&encoded, state.codecs()) {
        Ok(decoded) => Json(decoded).into_response(),
        Err(status) => (status, "Failed to decode track").into_response(),
    }
}

pub(crate) async fn decodetracks(
    State(state): State<AppState>,
    ApiJson(encoded_tracks): ApiJson<Vec<String>>,
) -> Response {
    if encoded_tracks.is_empty() {
        return (StatusCode::BAD_REQUEST, "No tracks to decode provided").into_response();
    }
    let mut tracks = Vec::with_capacity(encoded_tracks.len());
    for encoded in &encoded_tracks {
        match decode_one(encoded, state.codecs()) {
            Ok(decoded) => tracks.push(decoded),
            Err(status) => return (status, "Failed to decode track").into_response(),
        }
    }
    Json(tracks).into_response()
}

#[cfg(test)]
#[path = "../tests/tracks_decode.rs"]
mod tests;
