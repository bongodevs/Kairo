use std::collections::BTreeMap;

use super::*;
#[test]
fn absent_and_true_settings_stay_enabled_while_false_settings_disappear() {
    let settings = BTreeMap::from([
        (String::from("volume"), false),
        (String::from("channelMix"), true),
        (String::from("high-pass"), false),
        (String::from("unknown"), false),
    ]);
    let policy = FilterPolicy::configured(&settings);
    let names: Vec<_> = policy.enabled_names().collect();

    assert!(!names.contains(&"volume"));
    assert!(!names.contains(&"high-pass"));
    assert!(names.contains(&"channelMix"));
    assert!(names.contains(&"echo"));
    assert!(policy.allows("unknown"));
}

#[test]
fn the_default_policy_enables_the_whole_native_catalog() {
    assert_eq!(FilterPolicy::default().enabled_names().collect::<Vec<_>>(), FILTER_NAMES);
}
