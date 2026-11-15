use axum::response::IntoResponse;

use player::decode::Millis;
use player::source::{SourceError, SourceReference};
use player::track::{Playlist, SourceData, TrackBuilder, TrackInfo};

use super::*;
use crate::test_support::app_state;
fn track(identifier: &str) -> Track {
    Track::from_parts(
        SourceReference::new(identifier),
        TrackInfo {
            identifier: identifier.to_owned(),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(1_000)),
            is_stream: false,
            title: String::from("Title"),
            uri: Some(format!("https://example.test/{identifier}")),
            source_name: String::from("kairo"),
            artwork_url: None,
            isrc: None,
        },
        SourceData::None,
        Millis::default(),
    )
}

fn json(outcome: LoadOutcome) -> serde_json::Value {
    serde_json::to_value(to_response(outcome, &TrackCodecs::new()))
        .expect("serialize load response")
}

#[test]
fn every_load_outcome_has_the_v4_shape() {
    let base = track("one");
    let loaded_track =
        TrackBuilder::new(base.reference().clone(), base.info().expect("info").clone())
            .plugin_info(serde_json::Map::from_iter([
                (String::from("save_uri"), serde_json::json!("https://example.test/one")),
                (String::from("albumName"), serde_json::json!("Album")),
                (String::from("isPreview"), serde_json::json!(false)),
                (String::from("isLocal"), serde_json::json!(false)),
            ]))
            .build();
    let track_json = json(LoadOutcome::Track(loaded_track));
    assert_eq!(track_json["loadType"], "track");
    assert!(track_json["data"]["encoded"].is_string());
    assert_eq!(track_json["data"]["pluginInfo"]["albumName"], "Album");
    assert_eq!(track_json["data"]["pluginInfo"]["isLocal"], false);

    let playlist = Playlist::new("mix", vec![track("one"), track("two")])
        .with_plugin_info(serde_json::Map::from_iter([(
            String::from("type"),
            serde_json::json!("playlist"),
        )]))
        .with_selected_track(1)
        .expect("selection");
    let playlist = json(LoadOutcome::Playlist(playlist));
    assert_eq!(playlist["loadType"], "playlist");
    assert_eq!(playlist["data"]["info"]["name"], "mix");
    assert_eq!(playlist["data"]["info"]["selectedTrack"], 1);
    assert_eq!(playlist["data"]["pluginInfo"]["type"], "playlist");
    assert_eq!(playlist["data"]["tracks"].as_array().map(Vec::len), Some(2));

    let search = json(LoadOutcome::Search(vec![track("result")]));
    assert_eq!(search["loadType"], "search");
    assert!(search["data"].is_array());

    let empty = json(LoadOutcome::Empty);
    assert_eq!(empty, serde_json::json!({ "loadType": "empty", "data": null }));

    let error = json(LoadOutcome::Error(
        SourceError::load_failed("provider changed", ExceptionSeverity::Suspicious).into(),
    ));
    assert_eq!(error["loadType"], "error");
    assert_eq!(error["data"]["severity"], "suspicious");
    assert!(error["data"]["causeStackTrace"].is_string());
}

#[tokio::test]
async fn no_match_is_an_in_band_http_200() {
    let state = app_state(None);
    let response =
        loadtracks(State(state), ApiQuery(LoadQuery { identifier: String::from("missing") }))
            .await
            .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}
