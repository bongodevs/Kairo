use super::*;
use crate::SourceReference;
fn track(identifier: &str) -> Track {
    Track::new(SourceReference::new(identifier))
}

#[test]
fn selected_track_is_the_indexed_playlist_value() {
    let playlist = Playlist::new("mix", vec![track("first"), track("second")])
        .with_selected_track(1)
        .expect("selected track");

    assert_eq!(playlist.name(), "mix");
    assert_eq!(playlist.selected_track_index(), Some(1));
    assert_eq!(playlist.selected_track().expect("selected").reference().identifier(), "second");
    assert_eq!(playlist.kind(), PlaylistKind::Playlist);
}

#[test]
fn invalid_selection_is_rejected_without_changing_the_playlist() {
    let error = Playlist::new("mix", vec![track("only")])
        .with_selected_track(1)
        .expect_err("invalid selection");
    assert_eq!(error, PlaylistError::SelectedTrackOutOfBounds { index: 1, track_count: 1 });
}

#[test]
fn search_results_are_distinct_from_named_playlists() {
    let search = Playlist::search("query", vec![track("result")]);
    assert_eq!(search.kind(), PlaylistKind::Search);
    assert_eq!(search.selected_track_index(), None);
    assert_eq!(search.into_tracks().len(), 1);
}

#[test]
fn playlist_response_metadata_survives_selection_and_track_access() {
    let info = serde_json::Map::from_iter([
        (String::from("type"), serde_json::json!("album")),
        (String::from("totalTracks"), serde_json::json!(1)),
    ]);
    let playlist = Playlist::new("album", vec![track("one")])
        .with_plugin_info(info.clone())
        .with_selected_track(0)
        .expect("selection");
    assert_eq!(playlist.plugin_info(), &info);
    assert!(playlist.selected_track().is_some());
}
