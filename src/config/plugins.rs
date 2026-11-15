use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::Config;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    pub name: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub config: toml::Table,
}

impl PluginConfig {
    /// The plugin file for this entry, `<directory>/<name>.kai`.
    #[must_use]
    pub fn install_path(&self, directory: &Path) -> PathBuf {
        directory.join(format!("{}.{}", self.name, kairo_plugin_api::EXTENSION))
    }
}

impl fmt::Debug for PluginConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginConfig")
            .field("name", &self.name)
            .field("repo", &self.repo)
            .field("tag", &self.tag)
            .field("config", &(!self.config.is_empty()).then_some("<redacted>"))
            .finish()
    }
}

/// `[plugins]`: where installed plugin files live.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginDirectory {
    /// Holds `<name>.kai` and the `.run` staging area. Relative paths start at the working directory.
    #[serde(default = "default_plugin_directory")]
    pub directory: PathBuf,
}

impl Default for PluginDirectory {
    fn default() -> Self {
        Self { directory: default_plugin_directory() }
    }
}

fn default_plugin_directory() -> PathBuf {
    PathBuf::from("plugins")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackPlaceholder {
    Isrc,
    Query,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackProvider {
    pub source: String,
    pub template: String,
    pub placeholder: FallbackPlaceholder,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigValidationError {
    #[error("plugin name {name:?} must contain only lowercase letters, digits and dashes")]
    InvalidPluginName { name: String },
    #[error("plugin name {name:?} is configured more than once")]
    DuplicatePluginName { name: String },
    #[error("plugin {name:?} repository must be written as owner/name (got {repo:?})")]
    InvalidPluginRepo { name: String, repo: String },
    #[error("plugin {name:?} sets repo but has no tag")]
    MissingPluginTag { name: String },
    #[error("plugin {name:?} sets tag without repo")]
    PluginTagWithoutRepo { name: String },
    #[error("plugin {name:?} tag must not be empty")]
    EmptyPluginTag { name: String },
    #[error("plugins.directory must not be empty")]
    EmptyPluginDirectory,
    #[error(
        "fallback provider {entry:?} must be source:template with exactly one %ISRC% or %QUERY%"
    )]
    InvalidProvider { entry: String },
    #[error("fallback provider {entry:?} names unknown source {source_name:?}")]
    UnknownProvider { entry: String, source_name: String },
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.plugin_directory.directory.as_os_str().is_empty() {
            return Err(ConfigValidationError::EmptyPluginDirectory);
        }
        let mut names = BTreeSet::new();
        for plugin in &self.plugins {
            validate_plugin(plugin)?;
            if !names.insert(plugin.name.as_str()) {
                return Err(ConfigValidationError::DuplicatePluginName {
                    name: plugin.name.clone(),
                });
            }
        }
        Ok(())
    }
}

pub fn resolve_providers<'a>(
    providers: &[String],
    sources: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<FallbackProvider>, ConfigValidationError> {
    let sources: BTreeSet<&str> = sources.into_iter().collect();
    providers.iter().map(|entry| parse_provider(entry, &sources)).collect()
}

fn validate_plugin(plugin: &PluginConfig) -> Result<(), ConfigValidationError> {
    if plugin.name.is_empty()
        || !plugin
            .name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ConfigValidationError::InvalidPluginName { name: plugin.name.clone() });
    }
    if let Some(repo) = &plugin.repo
        && !valid_repo(repo)
    {
        return Err(ConfigValidationError::InvalidPluginRepo {
            name: plugin.name.clone(),
            repo: repo.clone(),
        });
    }
    match (&plugin.repo, &plugin.tag) {
        (Some(_), None) => {
            return Err(ConfigValidationError::MissingPluginTag { name: plugin.name.clone() });
        }
        (None, Some(_)) => {
            return Err(ConfigValidationError::PluginTagWithoutRepo { name: plugin.name.clone() });
        }
        (_, Some(tag)) if tag.trim().is_empty() => {
            return Err(ConfigValidationError::EmptyPluginTag { name: plugin.name.clone() });
        }
        _ => {}
    }
    Ok(())
}

fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if !owner.is_empty()
                && !name.is_empty()
                && owner.trim() == owner
                && name.trim() == name
    )
}

fn parse_provider(
    entry: &str,
    sources: &BTreeSet<&str>,
) -> Result<FallbackProvider, ConfigValidationError> {
    let Some((source, template)) = entry.split_once(':') else {
        return Err(ConfigValidationError::InvalidProvider { entry: entry.to_owned() });
    };
    let isrc = template.matches("%ISRC%").count();
    let query = template.matches("%QUERY%").count();
    let placeholder = match (isrc, query) {
        (1, 0) => FallbackPlaceholder::Isrc,
        (0, 1) => FallbackPlaceholder::Query,
        _ => return Err(ConfigValidationError::InvalidProvider { entry: entry.to_owned() }),
    };
    if source.is_empty() || template.is_empty() {
        return Err(ConfigValidationError::InvalidProvider { entry: entry.to_owned() });
    }
    if !sources.contains(source) {
        return Err(ConfigValidationError::UnknownProvider {
            entry: entry.to_owned(),
            source_name: source.to_owned(),
        });
    }
    Ok(FallbackProvider { source: source.to_owned(), template: template.to_owned(), placeholder })
}
