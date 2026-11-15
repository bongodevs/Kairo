use std::collections::BTreeMap;

use super::FiltersUpdate;
use crate::players::FilterPolicy;
#[test]
fn standard_and_builtin_plugin_filters_map_to_internal_state() {
    let update: FiltersUpdate = serde_json::from_value(serde_json::json!({
        "volume": 0.5,
        "equalizer": [{"band": 2, "gain": 0.25}],
        "timescale": {"speed": 1.2},
        "pluginFilters": {
            "high-pass": {"cutoffFrequency": 120, "boostFactor": 0.8},
            "normalization": {"maxAmplitude": 0.75},
            "unknown": {"x": 1}
        }
    }))
    .expect("filters");
    let (filters, wire) = update.into_parts().expect("convert");
    assert_eq!(filters.volume, 0.5);
    assert_eq!(filters.equalizer.gain(2), 0.25);
    assert_eq!(filters.timescale.expect("timescale").speed, 1.2);
    assert_eq!(filters.high_pass.expect("high pass").cutoff_hz, 120);
    assert_eq!(filters.normalization.expect("normalization").max_amplitude, 0.75);
    assert!(wire["pluginFilters"].get("high-pass").is_some());
    assert!(wire["pluginFilters"].get("normalization").is_some());
    assert!(wire["pluginFilters"].get("unknown").is_none());
}

#[test]
fn disabled_detection_includes_present_null_and_native_plugin_filters() {
    let update: FiltersUpdate = serde_json::from_value(serde_json::json!({
        "volume": 1.0,
        "karaoke": null,
        "pluginFilters": {"high-pass": {"cutoffFrequency": 120}}
    }))
    .expect("filters");
    let policy = FilterPolicy::configured(&BTreeMap::from([
        (String::from("volume"), false),
        (String::from("karaoke"), false),
        (String::from("high-pass"), false),
    ]));

    assert_eq!(update.disabled_by(policy), ["volume", "karaoke", "high-pass"]);
}

#[test]
fn only_nullable_filter_objects_accept_explicit_null() {
    for body in [
        serde_json::json!({"volume": null}),
        serde_json::json!({"equalizer": null}),
        serde_json::json!({"pluginFilters": null}),
    ] {
        assert!(serde_json::from_value::<FiltersUpdate>(body).is_err());
    }

    let update: FiltersUpdate = serde_json::from_value(serde_json::json!({
        "karaoke": null,
        "timescale": null
    }))
    .expect("nullable filters");
    let (filters, wire) = update.into_parts().expect("convert nullable filters");
    assert!(filters.karaoke.is_none());
    assert!(filters.timescale.is_none());
    assert!(wire.get("karaoke").is_none());
    assert!(wire.get("timescale").is_none());
}
