use std::sync::Arc;

use player::decode::Millis;
use player::source::ExtendedTrackCodec;
use player::source::SourceReference;
use player::track::{SourceData, Track, TrackBuilder, TrackInfo};

use super::*;
const SPOTIFY_TRACK: &str = "QAABZAMAL1lhIEhpdCAoZmVhdC4gRHJha2UsIFRyYXZpcyBTY290dCAmIExvZSBTaGltbXkpAAZGdXR1cmUAAAAAAAND8AAWNzZqUjVDOW9LYjFESlIzcmFiNmRHVgABADVodHRwczovL29wZW4uc3BvdGlmeS5jb20vdHJhY2svNzZqUjVDOW9LYjFESlIzcmFiNmRHVgEAQGh0dHBzOi8vaS5zY2RuLmNvL2ltYWdlL2FiNjc2MTZkMDAwMGIyNzM4YzQ1ODNiMWNiZjJiNTFlYzc5ZGQ2NmIBAAxVU1NNMTI2MDU0OTQAB3Nwb3RpZnkBAC9ZYSBIaXQgKGZlYXQuIERyYWtlLCBUcmF2aXMgU2NvdHQgJiBMb2UgU2hpbW15KQEANWh0dHBzOi8vb3Blbi5zcG90aWZ5LmNvbS9hbGJ1bS81S1hSSmRqUkJaTVBybXZ0NEVnWEFkAAAAAAAAAAAAAAAA";

fn sample_track() -> Track {
    Track::from_parts(
        SourceReference::new("abc"),
        TrackInfo {
            identifier: String::from("abc"),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(210_000)),
            is_stream: false,
            title: String::from("Song"),
            uri: Some(String::from("https://example/x")),
            source_name: String::from("kairo"),
            artwork_url: None,
            isrc: None,
        },
        SourceData::None,
        Millis::default(),
    )
}

#[test]
fn dto_has_the_reference_track_shape() {
    let encoded = "whatever";
    let base = sample_track();
    let track = TrackBuilder::new(base.reference().clone(), base.info().expect("info").clone())
        .plugin_info(serde_json::Map::from_iter([(
            String::from("albumName"),
            serde_json::json!("Album"),
        )]))
        .build();
    let value = serde_json::to_value(
        to_response(encoded, &track.snapshot(Millis::default())).expect("resolved response"),
    )
    .expect("serializable");
    assert_eq!(value["encoded"], "whatever");
    for key in [
        "identifier",
        "isSeekable",
        "author",
        "length",
        "isStream",
        "position",
        "title",
        "uri",
        "sourceName",
        "artworkUrl",
        "isrc",
    ] {
        assert!(value["info"].get(key).is_some(), "info missing {key}");
    }
    // isSeekable derives from !isStream; null optionals serialize as JSON null.
    assert_eq!(value["info"]["isSeekable"], true);
    assert!(value["info"]["artworkUrl"].is_null());
    assert_eq!(value["pluginInfo"], serde_json::json!({"albumName": "Album"}));
    // Empty userData is an object, not absent or an array.
    assert_eq!(value["userData"], serde_json::json!({}));
}

#[test]
fn stream_reported_as_unseekable() {
    let mut track = sample_track();
    let mut info = track.info().expect("info").clone();
    info.is_stream = true;
    info.is_seekable = false;
    track = Track::from_parts(track.reference().clone(), info, SourceData::None, Millis::default());
    let value = serde_json::to_value(
        to_response("x", &track.snapshot(Millis::default())).expect("resolved response"),
    )
    .expect("serializable");
    assert_eq!(value["info"]["isStream"], true);
    assert_eq!(value["info"]["isSeekable"], false);
}

#[test]
fn round_trip_encode_then_dto() {
    let track = sample_track();
    let codecs = TrackCodecs::new();
    let encoded = codec::encode(&track.snapshot(Millis::default()), &codecs).expect("encode");
    let dto = decode_one(&encoded, &codecs).expect("decode");
    assert_eq!(dto.info.title, "Song");
    assert_eq!(dto.info.uri.as_deref(), Some("https://example/x"));
}

#[test]
fn query_defaults_missing_fields_to_none() {
    let q: DecodeQuery =
        serde_json::from_value(serde_json::json!({ "track": "abc" })).expect("query");
    assert_eq!(q.encoded_track, None);
    assert_eq!(q.track.as_deref(), Some("abc"));
}

#[test]
fn unknown_source_decodes_opaque_but_invalid_base64_is_a_server_error() {
    use player::track::codec::OpaqueSourceCodec;

    let mut codecs = TrackCodecs::new();
    codecs.register(OpaqueSourceCodec::new("custom-source", 1)).expect("register");
    let mut info = sample_track().info().expect("info").clone();
    info.source_name = String::from("custom-source");
    let track = Track::from_parts(
        SourceReference::new("abc"),
        info,
        SourceData::Opaque(Arc::from([7_u8])),
        Millis::default(),
    );
    let encoded = codec::encode(&track.snapshot(Millis::default()), &codecs).expect("encode");
    let decoded = decode_one(&encoded, &TrackCodecs::new()).expect("opaque decode");
    assert_eq!(decoded.info.source_name, "custom-source");
    assert!(decoded.plugin_info.is_empty());
    assert!(matches!(
        decode_one("not-base64", &TrackCodecs::new()),
        Err(StatusCode::INTERNAL_SERVER_ERROR)
    ));
}

#[test]
fn unknown_spotify_tracks_decode_and_round_trip_byte_for_byte() {
    let mut codecs = TrackCodecs::new();
    codecs.set_fallback(ExtendedTrackCodec);

    let DecodeTrack::Track(track) = codec::decode(SPOTIFY_TRACK, &codecs).expect("decode track")
    else {
        panic!("expected track");
    };
    assert_eq!(track.info().expect("info").source_name, "spotify");
    let plugin_info = track.plugin_info();
    assert_eq!(plugin_info["albumName"], "Ya Hit (feat. Drake, Travis Scott & Loe Shimmy)");
    assert_eq!(plugin_info["albumUrl"], "https://open.spotify.com/album/5KXRJdjRBZMPrmvt4EgXAd");
    assert_eq!(plugin_info["save_uri"], "https://open.spotify.com/track/76jR5C9oKb1DJR3rab6dGV");
    assert_eq!(plugin_info["isPreview"], false);
    assert_eq!(plugin_info["isLocal"], false);
    for key in ["artistUrl", "artistArtworkUrl", "previewUrl"] {
        assert!(plugin_info[key].is_null(), "{key} should be null");
    }

    let encoded =
        codec::encode(&track.snapshot(track.initial_position()), &codecs).expect("re-encode");
    assert_eq!(encoded, SPOTIFY_TRACK);

    let dto = decode_one(SPOTIFY_TRACK, &codecs).expect("decode response");
    assert_eq!(dto.info.source_name, "spotify");
    assert_eq!(dto.info.identifier, "76jR5C9oKb1DJR3rab6dGV");
    assert_eq!(dto.info.title, "Ya Hit (feat. Drake, Travis Scott & Loe Shimmy)");
    assert_eq!(dto.plugin_info["isPreview"], false);
}
