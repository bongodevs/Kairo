use serde_json::{Map, Value};

use super::*;
use crate::{Millis, SourceReference, StreamInfo, TrackMetadata};
#[test]
fn unresolved_track_becomes_a_complete_snapshot_after_probe() {
    let reference = SourceReference::new("https://example.test/audio");
    let mut track = Track::new(reference.clone());
    assert!(track.info().is_none());

    track.resolve(
        TrackInfo::from_stream(&StreamInfo {
            source_name: String::from("http"),
            identifier: String::from("https://example.test/audio"),
            codec: "vorbis",
            container: "ogg",
            native_format: None,
            duration: Some(Millis::new(12_345)),
            seekable: true,
            metadata: TrackMetadata {
                title: Some(String::from("Title")),
                artist: Some(String::from("Artist")),
                artwork_url: Some(String::from("https://example.test/image")),
                isrc: Some(String::from("US123")),
            },
        }),
        SourceData::probe("ogg", None),
    );
    let snapshot = track.snapshot(Millis::new(321));
    let info = snapshot.info().expect("resolved info");
    assert_eq!(snapshot.reference(), &reference);
    assert_eq!(snapshot.position(), Millis::new(321));
    assert_eq!(info.title, "Title");
    assert_eq!(info.author, "Artist");
    assert_eq!(info.length, Some(Millis::new(12_345)));
    assert_eq!(info.uri.as_deref(), Some(reference.identifier()));
    assert_eq!(info.artwork_url.as_deref(), Some("https://example.test/image"));
    assert_eq!(info.isrc.as_deref(), Some("US123"));
    assert!(info.is_seekable);
    assert!(!info.is_stream);
}

#[test]
fn missing_metadata_uses_the_wire_fallbacks() {
    let info = TrackInfo::from_stream(&StreamInfo {
        source_name: String::from("http"),
        identifier: String::from("icy://radio"),
        codec: "mp3",
        container: "mp3",
        native_format: None,
        duration: None,
        seekable: false,
        metadata: TrackMetadata::default(),
    });
    assert_eq!(info.title, UNKNOWN_TITLE);
    assert_eq!(info.author, UNKNOWN_AUTHOR);
    assert!(info.is_stream);
    assert!(!info.is_seekable);
}

#[test]
fn stream_is_defined_by_unknown_length_not_by_seekability() {
    let mut stream = StreamInfo {
        source_name: String::from("http"),
        identifier: String::from("https://example.test/live"),
        codec: "mp3",
        container: "mp3",
        native_format: None,
        duration: Some(Millis::new(30_000)),
        seekable: false,
        metadata: TrackMetadata::default(),
    };
    let finite = TrackInfo::from_stream(&stream);
    assert!(!finite.is_seekable);
    assert!(!finite.is_stream, "a finite forward-only source is not a stream");

    stream.duration = None;
    stream.seekable = true;
    let live = TrackInfo::from_stream(&stream);
    assert!(live.is_seekable);
    assert!(live.is_stream, "an unknown length is a stream even when it can seek");
}

#[test]
fn replay_requires_an_explicit_fresh_track_value() {
    let track = Track::new(SourceReference::new("track"));
    let replay = track.make_clone();
    let from_event = track.snapshot(Millis::default()).make_clone();
    assert_eq!(replay.reference(), track.reference());
    assert_eq!(from_event.reference(), track.reference());
}

#[test]
fn response_and_user_data_are_preserved_by_snapshots_and_replay_values() {
    let plugin_info =
        Map::from_iter([(String::from("albumName"), Value::String(String::from("Album")))]);
    let mut user_data = Map::new();
    user_data.insert(String::from("requester"), Value::String(String::from("42")));
    let track = TrackBuilder::new(
        SourceReference::new("track"),
        TrackInfo {
            identifier: String::from("track"),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(1_000)),
            is_stream: false,
            title: String::from("Title"),
            uri: None,
            source_name: String::from("custom"),
            artwork_url: None,
            isrc: None,
        },
    )
    .source_data(SourceData::probe("custom", Some(String::from("v1"))))
    .plugin_info(plugin_info.clone())
    .user_data(user_data.clone())
    .initial_position(Millis::new(250))
    .build();
    let snapshot = track.snapshot(Millis::default());
    assert_eq!(track.initial_position(), Millis::new(250));
    assert_eq!(track.source_data(), &SourceData::probe("custom", Some(String::from("v1"))));
    assert_eq!(snapshot.plugin_info(), &plugin_info);
    assert_eq!(snapshot.user_data(), &user_data);
    assert_eq!(snapshot.make_clone().plugin_info(), &plugin_info);
    assert_eq!(snapshot.make_clone().user_data(), &user_data);
}
