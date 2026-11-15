use std::collections::BTreeMap;

pub(crate) const FILTER_NAMES: [&str; 14] = [
    "volume",
    "equalizer",
    "karaoke",
    "timescale",
    "tremolo",
    "vibrato",
    "distortion",
    "rotation",
    "channelMix",
    "lowPass",
    "high-pass",
    "low-pass",
    "normalization",
    "echo",
];

const ALL_ENABLED: u16 = (1 << FILTER_NAMES.len()) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FilterPolicy {
    enabled: u16,
}

impl FilterPolicy {
    pub(crate) fn configured(settings: &BTreeMap<String, bool>) -> Self {
        let mut policy = Self::default();
        for (index, name) in FILTER_NAMES.iter().enumerate() {
            if settings.get(*name) == Some(&false) {
                policy.enabled &= !bit(index);
            }
        }
        policy
    }

    pub(crate) fn allows(self, name: &str) -> bool {
        FILTER_NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .is_none_or(|index| self.enabled & bit(index) != 0)
    }

    pub(crate) fn enabled_names(self) -> impl Iterator<Item = &'static str> {
        FILTER_NAMES
            .into_iter()
            .enumerate()
            .filter_map(move |(index, name)| (self.enabled & bit(index) != 0).then_some(name))
    }
}

impl Default for FilterPolicy {
    fn default() -> Self {
        Self { enabled: ALL_ENABLED }
    }
}

const fn bit(index: usize) -> u16 {
    1 << index
}

#[cfg(test)]
#[path = "../../tests/filter_policy.rs"]
mod tests;
