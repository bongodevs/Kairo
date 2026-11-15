use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::filters::FiltersUpdate;
pub(super) use super::super::presence::Omissible;
use super::super::registry::VoiceState;
use super::super::settings::{CrossfadeSettings, TapeSettings};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PatchQuery {
    #[serde(default)]
    pub(super) no_replace: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayerUpdate {
    #[serde(default)]
    pub(super) track: Omissible<TrackUpdate>,
    #[serde(default)]
    pub(super) encoded_track: Omissible<Option<String>>,
    #[serde(default)]
    pub(super) identifier: Omissible<String>,
    #[serde(default)]
    pub(super) position: Omissible<u64>,
    #[serde(default)]
    pub(super) end_time: Omissible<Option<u64>>,
    #[serde(default)]
    pub(super) volume: Omissible<i32>,
    #[serde(default)]
    pub(super) paused: Omissible<bool>,
    #[serde(default)]
    pub(super) filters: Omissible<FiltersUpdate>,
    #[serde(default)]
    pub(super) voice: Omissible<VoiceState>,
    #[serde(default)]
    pub(super) next_track: Omissible<Option<TrackUpdate>>,
    #[serde(default)]
    pub(super) crossfade: Omissible<Option<CrossfadeSettings>>,
    #[serde(default)]
    pub(super) tape: Omissible<Option<TapeSettings>>,
    #[serde(default)]
    pub(super) transition: Omissible<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TrackUpdate {
    #[serde(default)]
    pub(super) encoded: Omissible<Option<String>>,
    #[serde(default)]
    pub(super) identifier: Omissible<String>,
    #[serde(default)]
    pub(super) user_data: Omissible<Map<String, Value>>,
}
