use std::path::Path;

use super::*;

fn parse(text: &str) -> Config {
    toml::from_str(text).expect("plugin config")
}

#[test]
fn absent_plugin_sections_have_empty_defaults() {
    let config = parse("");

    assert!(config.plugins.is_empty());
    assert!(config.sources.providers.is_empty());
    config.validate().expect("empty plugin config");
}

#[test]
fn plugin_entries_keep_opaque_tables_without_debugging_secrets() {
    let config = parse(
        r#"
[[plugin]]
name = "hello-source"
repo = "owner/hello-source"
tag = "v1.2.0"

[plugin.config]
api-key = "secret-value"
regions = ["us", "eu"]

[plugin.config.auth]
token = "hidden-token"

[sources]
providers = ["hello:%ISRC%", "http:search:%QUERY%"]
"#,
    );

    config.validate().expect("valid plugins");
    let plugin = &config.plugins[0];
    assert_eq!(plugin.name, "hello-source");
    assert_eq!(plugin.repo.as_deref(), Some("owner/hello-source"));
    assert_eq!(plugin.tag.as_deref(), Some("v1.2.0"));
    assert_eq!(plugin.config["api-key"].as_str(), Some("secret-value"));
    assert_eq!(plugin.config["auth"]["token"].as_str(), Some("hidden-token"));
    let debug = format!("{plugin:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("secret-value"));
    assert!(!debug.contains("hidden-token"));
}

#[test]
fn plugin_validation_names_the_invalid_entry() {
    let cases = [
        (
            "[[plugin]]\nname = \"Bad_Name\"\n",
            "must contain only lowercase letters, digits and dashes",
        ),
        ("[[plugin]]\nname = \"demo\"\nrepo = \"owner/demo\"\n", "sets repo but has no tag"),
        ("[[plugin]]\nname = \"demo\"\ntag = \"v1\"\n", "sets tag without repo"),
        ("[[plugin]]\nname = \"demo\"\nrepo = \"owner/extra/demo\"\ntag = \"v1\"\n", "owner/name"),
        (
            "[[plugin]]\nname = \"demo\"\nrepo = \"owner/demo\"\ntag = \" \"\n",
            "tag must not be empty",
        ),
        ("[[plugin]]\nname = \"demo\"\n[[plugin]]\nname = \"demo\"\n", "configured more than once"),
    ];

    for (document, message) in cases {
        let error = parse(document).validate().expect_err("invalid plugin config");
        assert!(error.to_string().contains(message), "{error}");
    }
}

#[test]
fn removed_plugin_keys_are_rejected_instead_of_ignored() {
    for document in [
        "[[plugin]]\nname = \"demo\"\nasset = \"demo.so\"\n",
        "[[plugin]]\nname = \"demo\"\nsha256 = \"abc\"\n",
        "[[plugin]]\nname = \"demo\"\nallow-unverified = true\n",
        "[[plugin]]\nname = \"demo\"\nfile = \"demo.so\"\n",
        "[[plugin]]\nname = \"demo\"\nenabled = false\n",
    ] {
        assert!(toml::from_str::<Config>(document).is_err(), "{document}");
    }
}

#[test]
fn config_load_runs_plugin_validation() {
    let path =
        std::env::temp_dir().join(format!("kairo-plugin-config-{}.toml", std::process::id()));
    std::fs::write(&path, "[[plugin]]\nname = \"UPPER\"\n").expect("write config");
    let error = Config::load(&path).expect_err("invalid plugin config");
    std::fs::remove_file(&path).expect("remove config");

    assert!(matches!(error, ConfigError::Validation { .. }));
    assert!(error.to_string().contains(path.file_name().unwrap().to_str().unwrap()));
}

#[test]
fn install_path_uses_the_plugin_extension_on_every_os() {
    let mut config = parse("[[plugin]]\nname = \"hello-source\"\n");
    config.validate().expect("local plugin");
    let plugin = config.plugins.remove(0);
    let path = plugin.install_path(Path::new("plugins"));

    assert_eq!(path, Path::new("plugins").join("hello-source.kai"));
}

#[test]
fn a_plugin_file_lives_in_the_configured_directory() {
    let mut config = parse("[[plugin]]\nname = \"hello-source\"\n");
    config.validate().expect("local plugin");
    let plugin = config.plugins.remove(0);

    assert_eq!(
        plugin.install_path(Path::new("build/plugins")),
        Path::new("build/plugins/hello-source.kai")
    );
}

#[test]
fn the_plugin_directory_defaults_to_plugins() {
    let config = parse("");

    assert_eq!(config.plugin_directory.directory, Path::new("plugins"));
}

#[test]
fn the_plugin_directory_is_configurable() {
    let config = parse("[plugins]\ndirectory = \"build/plugins\"\n");
    config.validate().expect("plugin directory");

    assert_eq!(config.plugin_directory.directory, Path::new("build/plugins"));
}

#[test]
fn an_empty_plugin_directory_is_rejected() {
    let config = parse("[plugins]\ndirectory = \"\"\n");
    let error = config.validate().expect_err("empty directory");
    assert!(error.to_string().contains("plugins.directory"));
}

#[test]
fn fallback_providers_are_typed_after_sources_are_known() {
    let config = parse(
        r#"
[sources]
providers = ["youtube:%ISRC%", "soundcloud:search:%QUERY%"]
"#,
    );
    let providers = resolve_providers(&config.sources.providers, ["http", "youtube", "soundcloud"])
        .expect("known providers");

    assert_eq!(providers[0].source, "youtube");
    assert_eq!(providers[0].template, "%ISRC%");
    assert_eq!(providers[0].placeholder, FallbackPlaceholder::Isrc);
    assert_eq!(providers[1].source, "soundcloud");
    assert_eq!(providers[1].template, "search:%QUERY%");
    assert_eq!(providers[1].placeholder, FallbackPlaceholder::Query);
}

#[test]
fn fallback_provider_syntax_rejects_missing_or_repeated_placeholders() {
    for entry in [
        "youtube",
        ":%QUERY%",
        "youtube:",
        "youtube:plain",
        "youtube:%ISRC%:%QUERY%",
        "youtube:%ISRC%/%ISRC%",
    ] {
        let error = resolve_providers(&[entry.into()], ["youtube"]).expect_err("invalid provider");
        assert!(matches!(error, ConfigValidationError::InvalidProvider { .. }), "{entry}");
    }
}

#[test]
fn unknown_fallback_source_is_deferred_until_manifests_are_known() {
    let config = parse("[sources]\nproviders = [\"future:%QUERY%\"]\n");
    config.validate().expect("plugin-independent validation");
    let error = resolve_providers(&config.sources.providers, ["http"]).expect_err("unknown source");

    assert!(matches!(
        error,
        ConfigValidationError::UnknownProvider { source_name, .. } if source_name == "future"
    ));
}
