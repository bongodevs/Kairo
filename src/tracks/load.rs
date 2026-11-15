use std::backtrace::Backtrace;
use std::error::Error;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use player::ExceptionSeverity;
use player::source::{LoadError, LoadOutcome, SourceReference};
use player::track::Track;
use player::track::codec::{TrackCodecError, TrackCodecs};

use super::dto::{TrackResponse, encode_track};
use crate::app::AppState;
use crate::rest::extract::ApiQuery;

#[derive(Debug, Deserialize)]
pub(crate) struct LoadQuery {
    identifier: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadResponse {
    load_type: LoadType,
    data: Option<LoadData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum LoadType {
    Track,
    Playlist,
    Search,
    Empty,
    Error,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum LoadData {
    Track(TrackResponse),
    Playlist(PlaylistResponse),
    Search(Vec<TrackResponse>),
    Error(ExceptionResponse),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistResponse {
    info: PlaylistInfoResponse,
    plugin_info: Map<String, Value>,
    tracks: Vec<TrackResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistInfoResponse {
    name: String,
    selected_track: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExceptionResponse {
    message: Option<String>,
    severity: SeverityResponse,
    cause: String,
    cause_stack_trace: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum SeverityResponse {
    Common,
    Suspicious,
    Fault,
}

pub(crate) async fn loadtracks(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<LoadQuery>,
) -> impl axum::response::IntoResponse {
    let outcome = state.sources().load(SourceReference::new(query.identifier)).await;
    if state.metrics_endpoint.is_some() {
        let (source, result) = load_result(&outcome);
        state.search_metrics.record_load(source, result);
    }
    Json(to_response(outcome, state.codecs()))
}

/// The source and result labels the reference's dev `SearchMetrics` attaches to a load.
fn load_result(outcome: &LoadOutcome) -> (&str, &'static str) {
    match outcome {
        LoadOutcome::Track(track) => (super::source_name(track), "track"),
        LoadOutcome::Playlist(playlist) => {
            let source = playlist.tracks().first().map_or("", super::source_name);
            (source, "playlist")
        }
        LoadOutcome::Search(tracks) => {
            let source = tracks.first().map_or("", super::source_name);
            (source, "search")
        }
        LoadOutcome::Empty => ("", "empty"),
        LoadOutcome::Error(_) => ("", "error"),
        _ => ("", "error"),
    }
}

fn to_response(outcome: LoadOutcome, codecs: &TrackCodecs) -> LoadResponse {
    match outcome {
        LoadOutcome::Track(track) => match encode_track(&track, codecs) {
            Ok(track) => response(LoadType::Track, LoadData::Track(track)),
            Err(error) => encoding_error(error),
        },
        LoadOutcome::Playlist(playlist) => {
            let selected_track = playlist
                .selected_track_index()
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1);
            match encode_tracks(playlist.tracks(), codecs) {
                Ok(tracks) => response(
                    LoadType::Playlist,
                    LoadData::Playlist(PlaylistResponse {
                        info: PlaylistInfoResponse {
                            name: playlist.name().to_owned(),
                            selected_track,
                        },
                        plugin_info: playlist.plugin_info().clone(),
                        tracks,
                    }),
                ),
                Err(error) => encoding_error(error),
            }
        }
        LoadOutcome::Search(tracks) => match encode_tracks(&tracks, codecs) {
            Ok(tracks) => response(LoadType::Search, LoadData::Search(tracks)),
            Err(error) => encoding_error(error),
        },
        LoadOutcome::Empty => LoadResponse { load_type: LoadType::Empty, data: None },
        LoadOutcome::Error(error) => load_error(error),
        _ => load_error(LoadError::message(
            "Identifier returned an unknown audio item type",
            ExceptionSeverity::Fault,
        )),
    }
}

fn encode_tracks(
    tracks: &[Track],
    codecs: &TrackCodecs,
) -> Result<Vec<TrackResponse>, TrackCodecError> {
    tracks.iter().map(|track| encode_track(track, codecs)).collect()
}

fn response(load_type: LoadType, data: LoadData) -> LoadResponse {
    LoadResponse { load_type, data: Some(data) }
}

fn load_error(error: LoadError) -> LoadResponse {
    let severity = error.severity();
    error_response(&error, severity)
}

fn encoding_error(error: TrackCodecError) -> LoadResponse {
    error_response(&error, ExceptionSeverity::Fault)
}

fn error_response(error: &(dyn Error + 'static), severity: ExceptionSeverity) -> LoadResponse {
    let mut cause = error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    response(
        LoadType::Error,
        LoadData::Error(ExceptionResponse {
            message: Some(error.to_string()),
            severity: match severity {
                ExceptionSeverity::Common => SeverityResponse::Common,
                ExceptionSeverity::Suspicious => SeverityResponse::Suspicious,
                ExceptionSeverity::Fault => SeverityResponse::Fault,
            },
            cause: cause.to_string(),
            cause_stack_trace: Backtrace::force_capture().to_string(),
        }),
    )
}

#[cfg(test)]
#[path = "../tests/jiosaavn_e2e.rs"]
mod jiosaavn_e2e;
#[cfg(test)]
#[path = "../tests/tracks_load.rs"]
mod tests;
