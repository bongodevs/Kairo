use serde_json::{Map, Value};
use thiserror::Error;

use super::instance::Track;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistKind {
    Playlist,
    Search,
}

#[derive(Debug)]
pub struct Playlist {
    name: String,
    tracks: Vec<Track>,
    selected_track: Option<usize>,
    kind: PlaylistKind,
    plugin_info: Map<String, Value>,
}

impl Playlist {
    #[must_use]
    pub fn new(name: impl Into<String>, tracks: Vec<Track>) -> Self {
        Self {
            name: name.into(),
            tracks,
            selected_track: None,
            kind: PlaylistKind::Playlist,
            plugin_info: Map::new(),
        }
    }

    #[must_use]
    pub fn search(name: impl Into<String>, tracks: Vec<Track>) -> Self {
        Self {
            name: name.into(),
            tracks,
            selected_track: None,
            kind: PlaylistKind::Search,
            plugin_info: Map::new(),
        }
    }

    #[must_use = "this returns the result of the operation, without modifying the original"]
    pub fn with_selected_track(mut self, index: usize) -> Result<Self, PlaylistError> {
        if index >= self.tracks.len() {
            return Err(PlaylistError::SelectedTrackOutOfBounds {
                index,
                track_count: self.tracks.len(),
            });
        }
        self.selected_track = Some(index);
        Ok(self)
    }

    #[must_use]
    pub fn with_plugin_info(mut self, plugin_info: Map<String, Value>) -> Self {
        self.plugin_info = plugin_info;
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    #[must_use]
    pub fn selected_track(&self) -> Option<&Track> {
        self.selected_track.and_then(|index| self.tracks.get(index))
    }

    #[must_use]
    pub const fn selected_track_index(&self) -> Option<usize> {
        self.selected_track
    }

    #[must_use]
    pub const fn kind(&self) -> PlaylistKind {
        self.kind
    }

    #[must_use]
    pub fn plugin_info(&self) -> &Map<String, Value> {
        &self.plugin_info
    }

    #[must_use]
    pub fn into_tracks(self) -> Vec<Track> {
        self.tracks
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlaylistError {
    #[error("selected track index {index} is outside a playlist of {track_count} tracks")]
    SelectedTrackOutOfBounds { index: usize, track_count: usize },
}
