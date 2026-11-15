use super::*;
use crate::players::FILTER_NAMES;
use crate::plugins::PluginManager;
#[test]
fn parses_plain_semver() {
    let v = parse_semver("4.0.7");
    assert_eq!((v.major, v.minor, v.patch), (4, 0, 7));
    assert_eq!(v.pre_release, None);
    assert_eq!(v.semver, "4.0.7");
}

#[test]
fn parses_prerelease_and_junk() {
    assert_eq!(parse_semver("1.2.3-beta.1").pre_release.as_deref(), Some("beta.1"));
    assert_eq!((parse_semver("x.y").major, parse_semver("x.y").minor), (0, 0));
}

#[test]
fn info_json_has_the_reference_keys() {
    let value = serde_json::to_value(build_info(
        &Sources::new(),
        FilterPolicy::default(),
        &PluginManager::empty(),
    ))
    .expect("serializable");
    for key in
        ["version", "buildTime", "git", "jvm", "lavaplayer", "sourceManagers", "filters", "plugins"]
    {
        assert!(value.get(key).is_some(), "missing {key}");
    }
    // `build`/`release`/`features` are NOT part of the reference wire shape.
    for key in ["build", "release", "features"] {
        assert!(value.get(key).is_none(), "unexpected {key}");
    }
    assert!(value["version"]["preRelease"].is_null());
    for key in ["branch", "commit", "commitTime"] {
        assert!(value["git"].get(key).is_some(), "git missing {key}");
    }
}

#[test]
fn info_reports_only_registered_source_names() {
    let sources = Sources::configured(
        player::source::SourceConfig { http: true, local: true },
        player::source::SourceCatalog::new(),
    )
    .expect("sources");
    assert_eq!(
        build_info(&sources, FilterPolicy::default(), &PluginManager::empty()).source_managers,
        ["local", "http"]
    );
}

#[test]
fn info_reports_every_native_filter_name() {
    assert_eq!(
        build_info(&Sources::new(), FilterPolicy::default(), &PluginManager::empty()).filters,
        FILTER_NAMES
    );
}

#[test]
fn build_metadata_is_a_millisecond_epoch_and_a_short_commit() {
    let info = build_info(&Sources::new(), FilterPolicy::default(), &PluginManager::empty());
    assert!(info.build_time > 0, "buildTime must be a millisecond epoch, not zero");

    let commit = info.git.commit;
    if commit != "unknown" {
        assert_eq!(commit.len(), 7, "the reference reports the abbreviated commit");
        assert!(commit.chars().all(|c| c.is_ascii_hexdigit()), "{commit}");
    }
}
