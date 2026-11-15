use kairo_plugin_api::{
    Playlist as WirePlaylist, Source as WireSource, TrackInfo as WireTrackInfo,
};
use player::track::PlaylistKind;
use serde_json::{Map, json};

use super::*;

fn source(name: &str, prefixes: &[&str], hosts: &[&str]) -> WireSource {
    WireSource {
        name: name.into(),
        prefixes: prefixes.iter().map(|value| (*value).into()).collect(),
        hosts: hosts.iter().map(|value| (*value).into()).collect(),
    }
}

fn track(source: &str) -> WireTrack {
    let info = WireTrackInfo::new("id", "Title", "Artist", source, 42_000);
    WireTrack::new(info)
}

#[test]
fn claims_match_prefix_exact_host_and_subdomain_only() {
    let claims = SourceClaims::new(&source("demo", &["demo"], &["Example.COM"]));

    assert!(claims.matches("demo:query"));
    assert!(claims.matches("https://example.com/item"));
    assert!(claims.matches("https://api.example.com/item"));
    assert!(!claims.matches("other:query"));
    assert!(!claims.matches("https://evil-example.com/item"));
    assert!(!claims.matches("ftp://example.com/item"));
}

#[test]
fn claim_validation_rejects_invalid_prefixes_and_hosts() {
    let prefix = validate_claims(&source("demo", &["Bad:Prefix"], &[])).unwrap_err();
    let host = validate_claims(&source("demo", &[], &["bad_host.test"])).unwrap_err();

    assert!(matches!(prefix, RegistrationError::InvalidPrefix { .. }));
    assert!(matches!(host, RegistrationError::InvalidHost { .. }));
}

#[test]
fn registry_rejects_name_prefix_and_overlapping_host_collisions() {
    let mut names = ClaimRegistry::new(["http"]);
    assert!(matches!(
        names.add(&source("http", &[], &[])),
        Err(RegistrationError::Collision { kind: "source name", .. })
    ));

    let mut prefixes = ClaimRegistry::new([]);
    prefixes.add(&source("one", &["find"], &[])).unwrap();
    assert!(matches!(
        prefixes.add(&source("two", &["find"], &[])),
        Err(RegistrationError::Collision { kind: "prefix", .. })
    ));

    let mut hosts = ClaimRegistry::new([]);
    hosts.add(&source("one", &[], &["example.com"])).unwrap();
    assert!(matches!(
        hosts.add(&source("two", &[], &["api.example.com"])),
        Err(RegistrationError::Collision { kind: "host", .. })
    ));
}

#[test]
fn plugin_track_becomes_one_native_track_without_losing_metadata() {
    let mut wire = track("demo").plugin_data("opaque").unwrap();
    wire.plugin_info = Map::from_iter([(String::from("album"), json!("Demo"))]);
    let SourceResult::Track(track) =
        convert_load("demo", "demo:id", LoadResult::Track(wire)).unwrap()
    else {
        panic!("expected track");
    };
    let info = track.info().expect("resolved info");

    assert_eq!(track.reference().identifier(), "id");
    assert_eq!(info.source_name, "demo");
    assert_eq!(info.length, Some(Millis::new(42_000)));
    assert_eq!(track.plugin_info()["album"], "Demo");
    let request = open_request("demo", &track).expect("open request");
    assert_eq!(request.source, "demo");
    assert_eq!(request.track.info.identifier, "id");
    assert_eq!(request.track.plugin_data_ref(), Some("opaque"));
    assert_eq!(request.track.plugin_info["album"], "Demo");
    let SourceData::Opaque(data) = track.source_data() else {
        panic!("expected opaque source state");
    };
    assert_eq!(data.as_ref(), b"opaque");
}

#[test]
fn stream_metadata_has_unknown_length_but_keeps_seekability() {
    let mut wire = track("demo");
    wire.info.is_stream = true;
    wire.info.is_seekable = false;
    let SourceResult::Track(track) =
        convert_load("demo", "demo:id", LoadResult::Track(wire)).unwrap()
    else {
        panic!("expected track");
    };
    let info = track.info().expect("resolved info");

    assert_eq!(info.length, None);
    assert!(info.is_stream);
    assert!(!info.is_seekable);
}

#[test]
fn search_and_playlist_keep_their_native_outcome_shapes() {
    let SourceResult::Playlist(search) =
        convert_load("demo", "demo:query", LoadResult::Search(vec![track("demo")])).unwrap()
    else {
        panic!("expected search playlist");
    };
    assert_eq!(search.kind(), PlaylistKind::Search);

    let playlist =
        WirePlaylist { name: "Mix".into(), selected_track: 0, tracks: vec![track("demo")] };
    let SourceResult::Playlist(playlist) =
        convert_load("demo", "demo:mix", LoadResult::Playlist(playlist)).unwrap()
    else {
        panic!("expected named playlist");
    };
    assert_eq!(playlist.kind(), PlaylistKind::Playlist);
    assert_eq!(playlist.selected_track_index(), Some(0));
}

#[test]
fn invalid_plugin_track_and_selection_are_source_faults() {
    let wrong_source = convert_load("demo", "demo:id", LoadResult::Track(track("other")))
        .expect_err("wrong source");
    assert_eq!(wrong_source.severity(), ExceptionSeverity::Fault);

    let playlist =
        WirePlaylist { name: "Mix".into(), selected_track: 2, tracks: vec![track("demo")] };
    let bad_selection = convert_load("demo", "demo:mix", LoadResult::Playlist(playlist))
        .expect_err("bad selection");
    assert_eq!(bad_selection.severity(), ExceptionSeverity::Fault);
}
