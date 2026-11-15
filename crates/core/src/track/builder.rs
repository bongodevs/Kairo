use serde_json::{Map, Value};

use super::info::TrackInfo;
use super::instance::{Track, TrackData};
use super::source_data::SourceData;
use crate::Millis;
use crate::SourceReference;

/// Builds one fully resolved track with its source payload and client state.
#[derive(Debug)]
#[must_use]
pub struct TrackBuilder {
    reference: SourceReference,
    info: TrackInfo,
    source_data: SourceData,
    plugin_info: Map<String, Value>,
    user_data: Map<String, Value>,
    initial_position: Millis,
}

impl TrackBuilder {
    pub fn new(reference: SourceReference, info: TrackInfo) -> Self {
        Self {
            reference,
            info,
            source_data: SourceData::None,
            plugin_info: Map::new(),
            user_data: Map::new(),
            initial_position: Millis::default(),
        }
    }

    pub fn source_data(mut self, source_data: SourceData) -> Self {
        self.source_data = source_data;
        self
    }

    pub fn plugin_info(mut self, plugin_info: Map<String, Value>) -> Self {
        self.plugin_info = plugin_info;
        self
    }

    pub fn user_data(mut self, user_data: Map<String, Value>) -> Self {
        self.user_data = user_data;
        self
    }

    pub const fn initial_position(mut self, initial_position: Millis) -> Self {
        self.initial_position = initial_position;
        self
    }

    #[must_use]
    pub fn build(self) -> Track {
        Track::from_data(TrackData {
            reference: self.reference,
            info: Some(self.info),
            source_data: self.source_data,
            plugin_info: self.plugin_info,
            encoded_position: self.initial_position,
            user_data: self.user_data,
            initial_position: self.initial_position,
        })
    }
}
