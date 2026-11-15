use serde::Serialize;
use serde_json::{Map, Value};

use player::track::codec::{self, TrackCodecError, TrackCodecs};
use player::track::{Track, TrackSnapshot};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrackResponse {
    pub(super) encoded: String,
    pub(super) info: TrackInfoResponse,
    pub(super) plugin_info: Map<String, Value>,
    pub(super) user_data: Map<String, Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TrackInfoResponse {
    pub(super) identifier: String,
    pub(super) is_seekable: bool,
    pub(super) author: String,
    pub(super) length: i64,
    pub(super) is_stream: bool,
    pub(super) position: i64,
    pub(super) title: String,
    pub(super) uri: Option<String>,
    pub(super) source_name: String,
    pub(super) artwork_url: Option<String>,
    pub(super) isrc: Option<String>,
}

pub(super) fn encode_track(
    track: &Track,
    codecs: &TrackCodecs,
) -> Result<TrackResponse, TrackCodecError> {
    let snapshot = track.snapshot(track.initial_position());
    let encoded = codec::encode(&snapshot, codecs)?;
    to_response(&encoded, &snapshot)
}

pub(crate) fn encode_snapshot(
    track: &TrackSnapshot,
    codecs: &TrackCodecs,
) -> Result<TrackResponse, TrackCodecError> {
    let encoded = codec::encode(track, codecs)?;
    to_response(&encoded, track)
}

pub(super) fn to_response(
    encoded: &str,
    track: &TrackSnapshot,
) -> Result<TrackResponse, TrackCodecError> {
    let info = track.info().ok_or(TrackCodecError::UnresolvedTrack)?;
    Ok(TrackResponse {
        encoded: encoded.to_owned(),
        info: TrackInfoResponse {
            identifier: info.identifier.clone(),
            is_seekable: info.is_seekable,
            author: info.author.clone(),
            length: info
                .length
                .map_or(i64::MAX, |length| i64::try_from(length.get()).unwrap_or(i64::MAX)),
            is_stream: info.is_stream,
            position: i64::try_from(track.position().get()).unwrap_or(i64::MAX),
            title: info.title.clone(),
            uri: info.uri.clone(),
            source_name: info.source_name.clone(),
            artwork_url: info.artwork_url.clone(),
            isrc: info.isrc.clone(),
        },
        plugin_info: track.plugin_info().clone(),
        user_data: track.user_data().clone(),
    })
}
