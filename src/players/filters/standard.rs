use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use player::filters::{EqualizerConfig, Filters};

use super::plugin::{Echo, Normalization, Pass};
use super::standard_wire::{
    Band, ChannelMix, Distortion, Karaoke, LowPass, Modulation, Rotation, Timescale,
};
use super::{FILTER_NAMES, FilterPolicy};
use crate::players::presence::Omissible;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::players) struct FiltersUpdate {
    #[serde(default, skip_serializing_if = "Omissible::is_omitted")]
    volume: Omissible<f32>,
    #[serde(default, skip_serializing_if = "Omissible::is_omitted")]
    equalizer: Omissible<Vec<Band>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    karaoke: Omissible<Option<Karaoke>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    timescale: Omissible<Option<Timescale>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    tremolo: Omissible<Option<Modulation>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    vibrato: Omissible<Option<Modulation>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    distortion: Omissible<Option<Distortion>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    rotation: Omissible<Option<Rotation>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    channel_mix: Omissible<Option<ChannelMix>>,
    #[serde(default, skip_serializing_if = "omitted_or_null")]
    low_pass: Omissible<Option<LowPass>>,
    #[serde(default)]
    plugin_filters: Map<String, Value>,
}

impl FiltersUpdate {
    pub(in crate::players) fn disabled_by(&self, policy: FilterPolicy) -> Vec<&'static str> {
        FILTER_NAMES
            .into_iter()
            .filter(|name| !policy.allows(name) && self.requests(name))
            .collect()
    }

    pub(in crate::players) fn into_parts(
        mut self,
    ) -> Result<(Filters, Map<String, Value>), String> {
        let high_pass = plugin::<Pass>(&self.plugin_filters, "high-pass")?;
        let cutoff_low_pass = plugin::<Pass>(&self.plugin_filters, "low-pass")?;
        let normalization = plugin::<Normalization>(&self.plugin_filters, "normalization")?;
        let echo = plugin::<Echo>(&self.plugin_filters, "echo")?;
        self.plugin_filters.retain(|name, _| {
            matches!(name.as_str(), "high-pass" | "low-pass" | "normalization" | "echo")
        });
        let wire = serde_json::to_value(&self)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .ok_or_else(|| String::from("failed to serialize filters"))?;
        let filters = Filters {
            volume: self.volume.into_option().unwrap_or(1.0),
            equalizer: EqualizerConfig::from_bands(
                self.equalizer
                    .into_option()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|band| (band.band, band.gain)),
            ),
            karaoke: self.karaoke.into_option().flatten().map(Into::into),
            timescale: self.timescale.into_option().flatten().map(Into::into),
            tremolo: self.tremolo.into_option().flatten().map(Into::into),
            vibrato: self.vibrato.into_option().flatten().map(Into::into),
            distortion: self.distortion.into_option().flatten().map(Into::into),
            rotation: self.rotation.into_option().flatten().map(Into::into),
            channel_mix: self.channel_mix.into_option().flatten().map(Into::into),
            low_pass: self.low_pass.into_option().flatten().map(Into::into),
            high_pass: high_pass.map(Into::into),
            cutoff_low_pass: cutoff_low_pass.map(Into::into),
            normalization: normalization.map(Into::into),
            echo: echo.map(Into::into),
        };
        filters.validate().map_err(|_| String::from("Invalid filter configuration"))?;
        Ok((filters, wire))
    }

    fn requests(&self, name: &str) -> bool {
        match name {
            "volume" => self.volume.is_present(),
            "equalizer" => self.equalizer.is_present(),
            "karaoke" => self.karaoke.is_present(),
            "timescale" => self.timescale.is_present(),
            "tremolo" => self.tremolo.is_present(),
            "vibrato" => self.vibrato.is_present(),
            "distortion" => self.distortion.is_present(),
            "rotation" => self.rotation.is_present(),
            "channelMix" => self.channel_mix.is_present(),
            "lowPass" => self.low_pass.is_present(),
            plugin => self.plugin_filters.contains_key(plugin),
        }
    }
}

fn omitted_or_null<T>(value: &Omissible<Option<T>>) -> bool {
    matches!(value, Omissible::Omitted | Omissible::Present(None))
}

fn plugin<T>(values: &Map<String, Value>, name: &str) -> Result<Option<T>, String>
where
    T: for<'de> Deserialize<'de>,
{
    values
        .get(name)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid {name} filter: {error}"))
}

#[cfg(test)]
#[path = "../../tests/player_filters.rs"]
mod tests;
