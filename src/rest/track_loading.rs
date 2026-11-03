//! Track loading, decoding, and categorized search.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use player::search::SearchType;
use player::{AudioPlayerManager, AudioTrack};

use crate::node::AppState;
use crate::protocol::load_result::{Exception, LoadResult, Playlist, PlaylistInfo};
use crate::protocol::search::SearchResult;
use crate::protocol::track::{Track, TrackInfo};
use crate::rest::error::{RestError, RestResult};
use crate::rest::json::JsonBody;

/// `GET /v4/loadtracks?identifier=...`, resolve an identifier into a [`LoadResult`].
///
/// Always responds `200`: a load failure is reported as `loadType: "error"`. A missing `identifier`
/// query is a `400`.
pub async fn load_tracks(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> RestResult<Json<LoadResult>> {
    let identifier = params
        .get("identifier")
        .ok_or_else(|| RestError::bad_request("Missing identifier query parameter"))?;

    tracing::info!(%identifier, "Got request to load identifier");

    let manager = state.manager();
    let result = match manager.load_item(identifier.clone()).await {
        player::LoadResult::Track(track) => {
            tracing::info!(title = %track.info().title, "Loaded track");
            match Track::from_engine(manager, track.as_ref()) {
                Ok(track) => LoadResult::Track(track),
                Err(err) => LoadResult::Error(Exception::from_friendly(&err)),
            }
        }
        player::LoadResult::Playlist(playlist) => {
            tracing::info!(name = %playlist.name, "Loaded playlist");
            let is_search = playlist.is_search_result;
            let selected = playlist.selected_track.map(|i| i as i32).unwrap_or(-1);
            let name = playlist.name.clone();
            match encode_all(manager, &playlist.tracks) {
                Ok(tracks) if is_search => LoadResult::Search(tracks),
                Ok(tracks) => LoadResult::Playlist(Playlist {
                    info: PlaylistInfo {
                        name,
                        selected_track: selected,
                    },
                    plugin_info: playlist.plugin_info,
                    tracks,
                }),
                Err(err) => LoadResult::Error(Exception::from_friendly(&err)),
            }
        }
        player::LoadResult::NoMatches => LoadResult::Empty,
        player::LoadResult::LoadFailed(err) => {
            // Log the full friendly cause chain, not just the user-facing message.
            // `err.message` alone ("HTTP request failed.") hides whether this was a
            // connect timeout, DNS failure, EMFILE, pool exhaustion or TLS error.
            // The underlying io kind is only available where the reqwest::Error is
            // still in scope (crates/player http.rs); here we log everything we kept.
            tracing::error!(
                identifier = %identifier,
                severity = %err.severity,
                message = %err.message,
                cause = err.cause.as_deref().unwrap_or("none"),
                cause_class = crate::rest::error_detail::classify(&err),
                "Failed to load track"
            );
            LoadResult::Error(Exception::from_friendly(&err))
        }
    };

    Ok(Json(result))
}

/// `GET /v4/decodetrack?encodedTrack=...`, or `?track=...`, to decode a single base64 track.
pub async fn decode_track(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> RestResult<Json<Track>> {
    let encoded = params
        .get("encodedTrack")
        .or_else(|| params.get("track"))
        .ok_or_else(|| RestError::bad_request("Missing encodedTrack query parameter"))?;

    let track = decode_one(state.manager(), encoded)?;
    Ok(Json(track))
}

/// `POST /v4/decodetracks`, decode a JSON array of base64 tracks. An empty array is a `400`.
pub async fn decode_tracks(
    State(state): State<AppState>,
    JsonBody(encoded): JsonBody<Vec<String>>,
) -> RestResult<Json<Vec<Track>>> {
    if encoded.is_empty() {
        return Err(RestError::bad_request("No tracks to decode provided"));
    }
    let manager = state.manager();
    let tracks = encoded
        .iter()
        .map(|encoded| decode_one(manager, encoded))
        .collect::<RestResult<Vec<_>>>()?;
    Ok(Json(tracks))
}

// Decode one base64 track, preserving the original encoded string.
//
// A full decode through the matching source manager comes first. When that source is not registered
// on this node, fall back to a metadata-only decode so clients can still read the track info; the
// track is not playable here until its source is added.
fn decode_one(manager: &AudioPlayerManager, encoded: &str) -> RestResult<Track> {
    if let Some(track) = manager
        .decode_track(encoded)
        .map_err(|err| RestError::from_friendly(&err))?
    {
        let info = TrackInfo::from_engine(track.info(), track.source_name(), 0);
        let mut proto = Track::new(encoded.to_string(), info);
        proto.plugin_info = track.plugin_info();
        return Ok(proto);
    }

    let decoded = manager
        .decode_track_info(encoded)
        .map_err(|err| RestError::from_friendly(&err))?;
    let info = TrackInfo::from_engine(&decoded.info, &decoded.source_name, 0);
    Ok(Track::new(encoded.to_string(), info))
}

fn encode_all(
    manager: &AudioPlayerManager,
    tracks: &[Box<dyn AudioTrack>],
) -> player::Result<Vec<Track>> {
    tracks
        .iter()
        .map(|track| Track::from_engine(manager, track.as_ref()))
        .collect()
}

/// `GET /v4/loadsearch?query=...&types=...`, a search split into categories.
///
/// `query` carries the source prefix, for instance `jssearch:foo` or `dzsearch:foo`, and is routed
/// to the one source that claims it. `types` is an optional comma-separated list of `track`,
/// `album`, `artist`, `playlist` and `text`; when absent every category is searched. No matching
/// source, or a search that yields nothing, gives `204 No Content` with no body.
pub async fn load_search(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> RestResult<Response> {
    let query = params
        .get("query")
        .ok_or_else(|| RestError::bad_request("Missing query parameter"))?;

    // Unknown type names are dropped, and an empty set searches everything.
    let types: Vec<SearchType> = params
        .get("types")
        .map(|raw| {
            raw.split(',')
                .filter_map(|part| SearchType::from_name(part.trim()))
                .collect()
        })
        .unwrap_or_default();

    let result = state.manager().load_search(query.clone(), types).await;
    if result.is_empty() {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let manager = state.manager();
    match SearchResult::from_engine(manager, result) {
        Ok(proto) => Ok(Json(proto).into_response()),
        Err(err) => Err(RestError::from_friendly(&err)),
    }
}
