use crate::decode::Millis;
use crate::filters::Filters;
use crate::track::TrackSnapshot;

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub track: Option<TrackSnapshot>,
    pub position: Millis,
    pub paused: bool,
    pub end_time: Option<Millis>,
    pub volume: u16,
    pub filters: Filters,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            track: None,
            position: Millis::default(),
            paused: false,
            end_time: None,
            volume: crate::volume::DEFAULT_VOLUME,
            filters: Filters::default(),
        }
    }
}
