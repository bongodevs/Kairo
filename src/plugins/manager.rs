use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use kairo_plugin_api::{HostCallbacks, LoadedPlugin, Manifest};
use player::source::{ProviderManager, Source};

use super::adapter::{ClaimRegistry, PluginSource, RegistrationError};
use super::codec::PluginCodec;
use super::download;
use super::host_api::{PluginHost, client};
use super::mirror::MirrorChain;
use super::native;
use super::stage;
use crate::config::{FallbackProvider, PluginConfig};

#[derive(Default)]
pub(crate) struct PluginManager {
    entries: Vec<Entry>,
}

struct Entry {
    name: String,
    state: EntryState,
}

enum EntryState {
    Loaded(Arc<LoadedPlugin>),
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PluginManagerError {
    #[error("build shared plugin HTTP client")]
    HttpClient(#[source] reqwest::Error),
}

impl PluginManager {
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(crate) async fn load(
        configs: &[PluginConfig],
        root: &Path,
    ) -> Result<Self, PluginManagerError> {
        if configs.is_empty() {
            return Ok(Self::empty());
        }
        let client = client().map_err(PluginManagerError::HttpClient)?;
        let downloader = download::client().map_err(PluginManagerError::HttpClient)?;
        let mut entries = Vec::with_capacity(configs.len());
        for config in configs {
            let state = match download::ensure(config, root, &downloader).await {
                Ok(()) => match load_one(config, root, client.clone()).await {
                    Ok(plugin) => {
                        tracing::info!(plugin = %config.name, version = %plugin.manifest().version, "plugin loaded");
                        EntryState::Loaded(plugin)
                    }
                    Err(reason) => {
                        tracing::error!(plugin = %config.name, %reason, "plugin failed to load");
                        EntryState::Failed(reason)
                    }
                },
                Err(error) => {
                    tracing::error!(plugin = %config.name, %error, "plugin download failed");
                    EntryState::Failed(error.to_string())
                }
            };
            entries.push(Entry { name: config.name.clone(), state });
        }
        Ok(Self { entries })
    }

    pub(crate) fn source_names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.state {
                EntryState::Loaded(plugin) => Some(plugin.as_ref()),
                EntryState::Failed(_) => None,
            })
            .flat_map(|plugin| plugin.manifest().sources.iter().map(|source| source.name.as_str()))
    }

    pub(crate) fn register_sources<'a>(
        &self,
        providers: &mut ProviderManager,
        reserved: impl IntoIterator<Item = &'a str>,
        fallbacks: Vec<FallbackProvider>,
    ) -> Result<Arc<MirrorChain>, RegistrationError> {
        let mirror = Arc::new(MirrorChain::new(fallbacks));
        if !self.entries.iter().any(|entry| matches!(&entry.state, EntryState::Loaded(_))) {
            return Ok(mirror);
        }
        let mut claims = ClaimRegistry::new(reserved);
        let http = providers.http_source()?;
        let mut adapters = Vec::new();

        for plugin in self.loaded() {
            for source in &plugin.manifest().sources {
                let source_claims = claims.add(source)?;
                adapters.push(PluginSource::new(
                    Arc::clone(plugin),
                    source,
                    source_claims,
                    http.clone(),
                    Arc::clone(&mirror),
                ));
            }
        }

        for adapter in adapters {
            let name = Source::name(&adapter).to_owned();
            providers.register(adapter, PluginCodec::new(name))?;
        }
        Ok(mirror)
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        let failed = self.failures().count();
        (self.entries.len() - failed, failed)
    }

    pub(crate) async fn shutdown(self) {
        if let Err(error) = tokio::task::spawn_blocking(move || {
            for entry in &self.entries {
                if let EntryState::Loaded(plugin) = &entry.state {
                    plugin.shutdown();
                }
            }
            drop(self);
        })
        .await
        {
            tracing::error!(%error, "plugin shutdown worker stopped");
        }
    }

    #[cfg(test)]
    pub(crate) fn failure(&self, name: &str) -> Option<&str> {
        self.failures().find_map(|(entry, reason)| (entry == name).then_some(reason))
    }

    fn failures(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter_map(|entry| match &entry.state {
            EntryState::Failed(reason) => Some((entry.name.as_str(), reason.as_str())),
            EntryState::Loaded(_) => None,
        })
    }

    fn loaded(&self) -> impl Iterator<Item = &Arc<LoadedPlugin>> {
        self.entries.iter().filter_map(|entry| match &entry.state {
            EntryState::Loaded(plugin) => Some(plugin),
            EntryState::Failed(_) => None,
        })
    }
}

async fn load_one(
    config: &PluginConfig,
    root: &Path,
    client: reqwest::Client,
) -> Result<Arc<LoadedPlugin>, String> {
    let path = stage::stage(config, root).await.map_err(|error| error.to_string())?;
    let init = serde_json::to_value(&config.config).map_err(|error| error.to_string())?;
    let callbacks: Arc<dyn HostCallbacks> = Arc::new(PluginHost::new(config.name.clone(), client));
    let expected_name = config.name.clone();
    let check_name = expected_name.clone();
    let plugin = tokio::task::spawn_blocking(move || {
        native::check(&path, &check_name).map_err(|error| error.to_string())?;
        LoadedPlugin::load(&path, callbacks, &init).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("plugin loader stopped: {error}"))??;
    if let Err(error) = validate_manifest(&expected_name, plugin.manifest()) {
        plugin.shutdown();
        return Err(error);
    }
    Ok(Arc::new(plugin))
}

fn validate_manifest(expected_name: &str, manifest: &Manifest) -> Result<(), String> {
    if manifest.name != expected_name {
        return Err(format!(
            "plugin manifest name {:?} does not match config name {expected_name:?}",
            manifest.name
        ));
    }
    if manifest.version.trim().is_empty() {
        return Err(String::from("plugin manifest version is empty"));
    }
    let mut sources = BTreeSet::new();
    for source in &manifest.sources {
        if !valid_name(&source.name) {
            return Err(format!("plugin source name {:?} is invalid", source.name));
        }
        if !sources.insert(source.name.as_str()) {
            return Err(format!("plugin source name {:?} is duplicated", source.name));
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
#[path = "../tests/plugins_manager.rs"]
mod tests;
