use serde_json::{Map, Value, json};

use super::*;

fn track_info() -> TrackInfo {
    TrackInfo {
        identifier: "id".into(),
        title: "A title".into(),
        author: "An author".into(),
        length: 42_000,
        is_stream: false,
        is_seekable: true,
        uri: Some("https://example.test/tracks/id".into()),
        artwork_url: Some("https://example.test/art/id.jpg".into()),
        isrc: Some("USRC17607839".into()),
        source_name: "demo".into(),
    }
}

fn track() -> Track {
    Track::new(track_info())
}

#[test]
fn manifest_keeps_sources_and_current_abi() {
    let manifest = Manifest {
        name: "demo-plugin".into(),
        version: "1.2.3".into(),
        abi: crate::ABI_VERSION,
        sources: vec![Source {
            name: "demo".into(),
            prefixes: vec!["demo".into()],
            hosts: vec!["example.test".into()],
        }],
    };

    assert_eq!(manifest.abi, crate::ABI_VERSION);
    assert_eq!(manifest.sources[0].prefixes, ["demo"]);
    assert_eq!(manifest.sources[0].hosts, ["example.test"]);
}

#[test]
fn manifest_ignores_additive_fields_and_defaults_claims() {
    let manifest: Manifest = serde_json::from_value(json!({
        "name": "demo-plugin",
        "version": "1.0.0",
        "abi": 1,
        "future": true,
        "sources": [{ "name": "demo", "future": 7 }]
    }))
    .unwrap();

    assert_eq!(
        manifest.sources[0],
        Source { name: "demo".into(), prefixes: Vec::new(), hosts: Vec::new() }
    );
}

#[test]
fn load_request_ignores_additive_fields() {
    let request: LoadRequest = serde_json::from_value(json!({
        "source": "demo",
        "identifier": "demo:hello",
        "future": true
    }))
    .unwrap();

    assert_eq!(request.source, "demo");
    assert_eq!(request.identifier, "demo:hello");
}

#[test]
fn track_info_uses_camel_case_and_keeps_optional_metadata() {
    let mut info = track_info();
    info.is_stream = true;
    info.is_seekable = false;
    let value = serde_json::to_value(info).unwrap();

    assert_eq!(value["sourceName"], "demo");
    assert_eq!(value["isStream"], true);
    assert_eq!(value["isSeekable"], false);
    assert_eq!(value["artworkUrl"], "https://example.test/art/id.jpg");
    assert_eq!(value["isrc"], "USRC17607839");
}

#[test]
fn plugin_data_accepts_exactly_four_kibibytes() {
    let payload = "é".repeat(MAX_PLUGIN_DATA_BYTES / 2);
    let track = track().plugin_data(payload.clone()).unwrap();
    let value = serde_json::to_value(&track).unwrap();
    let decoded: Track = serde_json::from_value(value).unwrap();

    assert_eq!(payload.len(), MAX_PLUGIN_DATA_BYTES);
    assert_eq!(decoded.plugin_data_ref(), Some(payload.as_str()));
}

#[test]
fn plugin_data_rejects_more_than_four_kibibytes() {
    let payload = "x".repeat(MAX_PLUGIN_DATA_BYTES + 1);
    let error = track().plugin_data(payload.clone()).unwrap_err();
    let value = json!({ "info": track_info(), "pluginData": payload });
    let decode_error = serde_json::from_value::<Track>(value).unwrap_err();

    assert_eq!(error.severity, Severity::Fault);
    assert!(error.to_string().contains("4097 bytes"));
    assert!(decode_error.to_string().contains("maximum is 4096"));
}

#[test]
fn absent_and_null_plugin_data_are_empty() {
    let missing: Track = serde_json::from_value(json!({ "info": track_info() })).unwrap();
    let null: Track =
        serde_json::from_value(json!({ "info": track_info(), "pluginData": null })).unwrap();

    assert_eq!(missing.plugin_data_ref(), None);
    assert_eq!(null.plugin_data_ref(), None);
    assert_eq!(track().into_parts().2, None);
}

#[test]
fn every_load_result_has_the_documented_tag_and_data() {
    let result = LoadResult::Track(track());
    let track_value = serde_json::to_value(result).unwrap();
    let search_value = serde_json::to_value(LoadResult::Search(vec![track()])).unwrap();
    let playlist_value = serde_json::to_value(LoadResult::Playlist(Playlist {
        name: "Mix".into(),
        selected_track: 0,
        tracks: vec![track()],
    }))
    .unwrap();
    let empty_value = serde_json::to_value(LoadResult::Empty(())).unwrap();

    assert_eq!(track_value["loadType"], "track");
    assert!(track_value["data"]["pluginInfo"].is_object());
    assert!(track_value["data"]["pluginData"].is_null());
    assert_eq!(search_value["loadType"], "search");
    assert!(search_value["data"].is_array());
    assert_eq!(playlist_value["data"]["selectedTrack"], 0);
    assert_eq!(empty_value, json!({ "loadType": "empty", "data": null }));
}

#[test]
fn load_result_ignores_unknown_fields_at_each_level() {
    let mut info = serde_json::to_value(track_info()).unwrap();
    info.as_object_mut().unwrap().insert("future".into(), Value::Bool(true));
    let result: LoadResult = serde_json::from_value(json!({
        "loadType": "track",
        "future": "ignored",
        "data": {
            "info": info,
            "pluginInfo": {},
            "pluginData": null,
            "future": [1, 2, 3]
        }
    }))
    .unwrap();

    let LoadResult::Track(track) = result else {
        panic!("expected track result");
    };
    assert_eq!(track.info.identifier, "id");
}

#[test]
fn open_url_carries_transport_hints() {
    let open = Open::Url {
        url: "https://cdn.example.test/id.opus".into(),
        headers: [("User-Agent".into(), "demo".into())].into(),
        seekable: Some(true),
        length_ms: Some(42_000),
        content_type: Some("audio/ogg".into()),
    };
    let value = serde_json::to_value(&open).unwrap();
    let mut with_future = value.clone();
    with_future.as_object_mut().unwrap().insert("future".into(), Value::String("ignored".into()));

    assert_eq!(value["kind"], "url");
    assert_eq!(value["headers"]["User-Agent"], "demo");
    assert_eq!(value["lengthMs"], 42_000);
    assert_eq!(serde_json::from_value::<Open>(with_future).unwrap(), open);
}

#[test]
fn open_mirror_carries_optional_matching_fields() {
    let open = Open::Mirror {
        isrc: Some("USRC17607839".into()),
        title: "A title".into(),
        author: "An author".into(),
        length_ms: Some(42_000),
    };
    let value = serde_json::to_value(open).unwrap();

    assert_eq!(value["kind"], "mirror");
    assert_eq!(value["isrc"], "USRC17607839");
    assert_eq!(value["lengthMs"], 42_000);
}

#[test]
fn open_request_round_trips_opaque_track_state() {
    let request = OpenRequest {
        source: "demo".into(),
        track: track().plugin_data("opaque").unwrap(),
        direct: false,
    };
    let value = serde_json::to_value(&request).unwrap();
    let decoded: OpenRequest = serde_json::from_value(value).unwrap();

    assert_eq!(decoded.source, "demo");
    assert_eq!(decoded.track.plugin_data_ref(), Some("opaque"));
    assert!(!decoded.direct);
}

#[test]
fn open_request_only_serializes_direct_when_requested() {
    let mut request = OpenRequest { source: "demo".into(), track: track(), direct: false };
    let value = serde_json::to_value(&request).unwrap();
    assert!(value.get("direct").is_none());

    request.direct = true;
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["direct"], true);
}

#[test]
fn plugin_error_keeps_severity_and_outage_state() {
    let common =
        PluginError { severity: Severity::Common, message: "not found".into(), unavailable: false };
    let unavailable = PluginError {
        severity: Severity::Suspicious,
        message: "provider is down".into(),
        unavailable: true,
    };

    assert_eq!(
        serde_json::to_value(&common).unwrap(),
        json!({
            "severity": "common",
            "message": "not found"
        })
    );
    assert_eq!(unavailable.severity, Severity::Suspicious);
    assert!(unavailable.unavailable);
    assert_eq!(common.to_string(), "not found");
}

#[test]
fn missing_unavailable_flag_defaults_to_false() {
    let error: PluginError = serde_json::from_value(json!({
        "severity": "common",
        "message": "not found",
        "future": true
    }))
    .unwrap();

    assert!(!error.unavailable);
}

#[test]
fn http_messages_use_uppercase_methods_and_default_headers() {
    let request: HttpRequest = serde_json::from_value(json!({
        "method": "GET",
        "url": "https://api.example.test/item",
        "body": null,
        "future": 1
    }))
    .unwrap();
    let response: HttpResponse = serde_json::from_value(json!({
        "status": 200,
        "body": "{}",
        "future": 2
    }))
    .unwrap();

    assert_eq!(request.method, HttpMethod::Get);
    assert!(request.headers.is_empty());
    assert!(response.headers.is_empty());
    assert_eq!(serde_json::to_value(HttpMethod::Post).unwrap(), "POST");
}

#[test]
fn plugin_info_remains_an_arbitrary_json_object() {
    let mut plugin_info = Map::new();
    plugin_info.insert("album".into(), json!({ "id": 7 }));
    let mut track = track();
    track.plugin_info = plugin_info.clone();

    assert_eq!(serde_json::to_value(track).unwrap()["pluginInfo"], Value::Object(plugin_info));
}
