use std::sync::Arc;

use serde_json::{Map, Value};

use super::info::TrackInfo;
use super::source_data::SourceData;
use crate::Millis;
use crate::SourceReference;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrackData {
    pub(super) reference: SourceReference,
    pub(super) info: Option<TrackInfo>,
    pub(super) source_data: SourceData,
    pub(super) plugin_info: Map<String, Value>,
    pub(super) initial_position: Millis,
    /// Carried by the wire encoding; it never follows playback, so one track keeps one identity.
    pub(super) encoded_position: Millis,
    pub(super) user_data: Map<String, Value>,
}

/// One playable track instance. Moving it into a player makes replay explicit.
#[derive(Debug)]
pub struct Track {
    pub(super) data: Arc<TrackData>,
}

impl Track {
    #[must_use]
    pub fn new(reference: SourceReference) -> Self {
        Self {
            data: Arc::new(TrackData {
                reference,
                info: None,
                source_data: SourceData::None,
                plugin_info: Map::new(),
                initial_position: Millis::default(),
                encoded_position: Millis::default(),
                user_data: Map::new(),
            }),
        }
    }

    #[must_use]
    pub fn with_info(reference: SourceReference, info: TrackInfo) -> Self {
        super::builder::TrackBuilder::new(reference, info).build()
    }

    #[must_use]
    pub fn reference(&self) -> &SourceReference {
        &self.data.reference
    }

    #[must_use]
    pub fn info(&self) -> Option<&TrackInfo> {
        self.data.info.as_ref()
    }

    #[must_use]
    pub fn source_data(&self) -> &SourceData {
        &self.data.source_data
    }

    #[must_use]
    pub fn plugin_info(&self) -> &Map<String, Value> {
        &self.data.plugin_info
    }

    #[must_use]
    pub fn initial_position(&self) -> Millis {
        self.data.initial_position
    }

    #[must_use]
    pub fn user_data(&self) -> &Map<String, Value> {
        &self.data.user_data
    }

    pub fn set_user_data(&mut self, user_data: Map<String, Value>) {
        Arc::make_mut(&mut self.data).user_data = user_data;
    }

    pub fn set_initial_position(&mut self, position: Millis) {
        Arc::make_mut(&mut self.data).initial_position = position;
    }

    #[must_use]
    pub fn make_clone(&self) -> Self {
        Self { data: Arc::clone(&self.data) }
    }

    #[must_use]
    pub fn snapshot(&self, position: Millis) -> TrackSnapshot {
        TrackSnapshot { data: Arc::clone(&self.data), position }
    }

    pub fn resolve(&mut self, info: TrackInfo, source_data: SourceData) {
        let data = Arc::make_mut(&mut self.data);
        data.info = Some(info);
        data.source_data = source_data;
    }

    #[must_use]
    pub fn from_parts(
        reference: SourceReference,
        info: TrackInfo,
        source_data: SourceData,
        initial_position: Millis,
    ) -> Self {
        super::builder::TrackBuilder::new(reference, info)
            .source_data(source_data)
            .initial_position(initial_position)
            .build()
    }

    pub(super) fn from_data(data: TrackData) -> Self {
        Self { data: Arc::new(data) }
    }
}

impl From<SourceReference> for Track {
    fn from(reference: SourceReference) -> Self {
        Self::new(reference)
    }
}

/// A track plus the playback position at the moment the snapshot was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSnapshot {
    data: Arc<TrackData>,
    position: Millis,
}

impl TrackSnapshot {
    #[must_use]
    pub fn reference(&self) -> &SourceReference {
        &self.data.reference
    }

    #[must_use]
    pub fn info(&self) -> Option<&TrackInfo> {
        self.data.info.as_ref()
    }

    #[must_use]
    pub fn source_data(&self) -> &SourceData {
        &self.data.source_data
    }

    #[must_use]
    pub fn plugin_info(&self) -> &Map<String, Value> {
        &self.data.plugin_info
    }

    #[must_use]
    pub fn initial_position(&self) -> Millis {
        self.data.initial_position
    }

    /// The fixed position written into the encoded track, unlike [`Self::position`].
    #[must_use]
    pub fn encoded_position(&self) -> Millis {
        self.data.encoded_position
    }

    #[must_use]
    pub fn user_data(&self) -> &Map<String, Value> {
        &self.data.user_data
    }

    #[must_use]
    pub const fn position(&self) -> Millis {
        self.position
    }

    #[must_use]
    pub fn make_clone(&self) -> Track {
        Track { data: Arc::clone(&self.data) }
    }
}

impl PartialEq<SourceReference> for TrackSnapshot {
    fn eq(&self, other: &SourceReference) -> bool {
        self.reference() == other
    }
}
