use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use kairo_plugin_api::{LoadedPlugin, Manifest};
use player::source::{ProviderManager, Source};
use serde::Serialize;

use super::adapter::{ClaimRegistry, PluginSource, RegistrationError};
use super::codec::PluginCodec;
use super::download;
use super::guard::{self, Startup};
use super::host_api::client;
use super::load::load_staged;
use super::mirror::MirrorChain;
use super::slot::PluginSlot;
use super::stage;
use crate::config::{FallbackProvider, PluginConfig};

/// How long a reload waits for calls that still hold the previous copy.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct PluginManager {
    entries: Vec<Entry>,
    root: PathBuf,
    client: Option<reqwest::Client>,
}

struct Entry {
    name: String,
    config: PluginConfig,
    state: RwLock<EntryState>,
    reload: tokio::sync::Mutex<()>,
}

impl Entry {
    fn state(&self) -> RwLockReadGuard<'_, EntryState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn state_mut(&self) -> RwLockWriteGuard<'_, EntryState> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Clone)]
enum EntryState {
    Loaded { slot: Arc<PluginSlot>, hash: String, reason: Option<String> },
    Failed { reason: String },
    Quarantined { since: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PluginEntry {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) sources: Vec<String>,
    pub(crate) state: &'static str,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReloadError {
    #[error("plugin {name:?} is not configured")]
    Unknown { name: String },
    #[error(
        "plugin {name:?} is quarantined since {since}; delete the guard marker and restart the server"
    )]
    Quarantined { name: String, since: u64 },
    #[error(
        "plugin {name:?} is not loaded this run and cannot be registered without a restart: {reason}"
    )]
    Unavailable { name: String, reason: String },
    #[error("plugin {name:?} reload failed: {reason}")]
    Failed { name: String, reason: String },
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
            let state = discover(config, root, &downloader, &client).await;
            entries.push(Entry {
                name: config.name.clone(),
                config: config.clone(),
                state: RwLock::new(state),
                reload: tokio::sync::Mutex::new(()),
            });
        }
        Ok(Self { entries, root: root.to_owned(), client: Some(client) })
    }

    pub(crate) fn source_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for entry in &self.entries {
            let state = entry.state();
            let EntryState::Loaded { slot, .. } = &*state else {
                continue;
            };
            names.extend(slot.plugin().manifest().sources.iter().map(|source| source.name.clone()));
        }
        names
    }

    pub(crate) fn register_sources<'a>(
        &self,
        providers: &mut ProviderManager,
        reserved: impl IntoIterator<Item = &'a str>,
        fallbacks: Vec<FallbackProvider>,
    ) -> Result<Arc<MirrorChain>, RegistrationError> {
        let mirror = Arc::new(MirrorChain::new(fallbacks));
        let mut claims = ClaimRegistry::new(reserved);
        let http = providers.http_source()?;
        let mut adapters = Vec::new();

        for entry in &self.entries {
            let state = entry.state();
            let EntryState::Loaded { slot, .. } = &*state else {
                continue;
            };
            let plugin = slot.plugin();
            for source in &plugin.manifest().sources {
                let source_claims = claims.add(source)?;
                adapters.push(PluginSource::new(
                    Arc::clone(slot),
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
        let mut loaded = 0;
        let mut failed = 0;
        for entry in &self.entries {
            match &*entry.state() {
                EntryState::Loaded { .. } => loaded += 1,
                EntryState::Failed { .. } | EntryState::Quarantined { .. } => failed += 1,
            }
        }
        (loaded, failed)
    }

    pub(crate) fn versions(&self) -> Vec<(String, String)> {
        let mut versions = Vec::new();
        for entry in &self.entries {
            let state = entry.state();
            let EntryState::Loaded { slot, .. } = &*state else {
                continue;
            };
            versions.push((entry.name.clone(), slot.plugin().manifest().version.clone()));
        }
        versions
    }

    pub(crate) fn listing(&self) -> Vec<PluginEntry> {
        self.entries.iter().map(entry_listing).collect()
    }

    pub(crate) async fn reload(&self, name: &str) -> Result<PluginEntry, ReloadError> {
        let Some(index) = self.entries.iter().position(|entry| entry.name == name) else {
            return Err(ReloadError::Unknown { name: name.to_owned() });
        };
        let entry = &self.entries[index];
        let _reload = entry.reload.lock().await;

        let current = entry.state().clone();
        let (slot, current_hash) = match current {
            EntryState::Loaded { slot, hash, .. } => (slot, hash),
            EntryState::Quarantined { since } => {
                return Err(ReloadError::Quarantined { name: name.to_owned(), since });
            }
            EntryState::Failed { reason } => {
                return Err(ReloadError::Unavailable { name: name.to_owned(), reason });
            }
        };
        let Some(client) = self.client.clone() else {
            return Err(ReloadError::Unavailable {
                name: name.to_owned(),
                reason: String::from("plugin HTTP client is not configured"),
            });
        };
        tracing::info!(plugin = name, "reloading plugin");

        let staged = match stage::stage(&entry.config, &self.root).await {
            Ok(staged) => staged,
            Err(error) => {
                let reason = error.to_string();
                record_failure(entry, reason.clone());
                return Err(ReloadError::Failed { name: name.to_owned(), reason });
            }
        };
        if let Err(error) = guard::arm(&self.root, name, &staged.hash).await {
            let reason = error.to_string();
            record_failure(entry, reason.clone());
            return Err(ReloadError::Failed { name: name.to_owned(), reason });
        }
        let plugin = match load_staged(&entry.config, &staged.path, client).await {
            Ok(plugin) => plugin,
            Err(reason) => {
                guard::disarm(&self.root, name).await;
                record_failure(entry, reason.clone());
                return Err(ReloadError::Failed { name: name.to_owned(), reason });
            }
        };
        let previous = slot.plugin();
        if let Err(reason) = validate_reload(name, previous.manifest(), plugin.manifest()) {
            plugin.shutdown();
            guard::disarm(&self.root, name).await;
            record_failure(entry, reason.clone());
            return Err(ReloadError::Failed { name: name.to_owned(), reason });
        }
        if staged.hash == current_hash {
            tracing::info!(plugin = name, "plugin reload reuses the staged copy");
        }

        // the swapped-out handle is one more reference to `previous`, which retire waits on
        drop(slot.replace(Arc::clone(&plugin)));
        *entry.state_mut() =
            EntryState::Loaded { slot: Arc::clone(&slot), hash: staged.hash.clone(), reason: None };
        guard::schedule_disarm(self.root.clone(), name.to_owned(), staged.hash);
        tracing::info!(
            plugin = name,
            version = %plugin.manifest().version,
            "plugin reloaded"
        );
        retire(previous, name).await;

        Ok(entry_listing(entry))
    }

    pub(crate) async fn shutdown(&self) {
        let mut plugins = Vec::new();
        for entry in &self.entries {
            let loaded = {
                let state = entry.state();
                if let EntryState::Loaded { slot, .. } = &*state {
                    plugins.push(slot.plugin());
                    true
                } else {
                    false
                }
            };
            if loaded {
                guard::disarm(&self.root, &entry.name).await;
            }
        }
        if plugins.is_empty() {
            return;
        }
        if let Err(error) = tokio::task::spawn_blocking(move || {
            for plugin in plugins {
                plugin.shutdown();
            }
        })
        .await
        {
            tracing::error!(%error, "plugin shutdown worker stopped");
        }
    }

    #[cfg(test)]
    pub(crate) fn failure(&self, name: &str) -> Option<String> {
        self.entries.iter().find(|entry| entry.name == name).and_then(|entry| {
            match &*entry.state() {
                EntryState::Failed { reason } => Some(reason.clone()),
                EntryState::Quarantined { since } => {
                    Some(format!("plugin is quarantined since {since}"))
                }
                EntryState::Loaded { .. } => None,
            }
        })
    }
}

fn entry_listing(entry: &Entry) -> PluginEntry {
    let state = entry.state();
    match &*state {
        EntryState::Loaded { slot, reason, .. } => {
            let manifest = slot.plugin().manifest().clone();
            PluginEntry {
                name: entry.name.clone(),
                version: Some(manifest.version),
                sources: manifest.sources.iter().map(|source| source.name.clone()).collect(),
                state: "loaded",
                reason: reason.clone(),
            }
        }
        EntryState::Failed { reason } => PluginEntry {
            name: entry.name.clone(),
            version: None,
            sources: Vec::new(),
            state: "failed",
            reason: Some(reason.clone()),
        },
        EntryState::Quarantined { since } => PluginEntry {
            name: entry.name.clone(),
            version: None,
            sources: Vec::new(),
            state: "quarantined",
            reason: Some(format!("quarantined since {since}")),
        },
    }
}

fn record_failure(entry: &Entry, reason: String) {
    if let EntryState::Loaded { reason: last, .. } = &mut *entry.state_mut() {
        *last = Some(reason);
    }
}

/// Wait for calls that already hold `previous`, then drop the last reference on a blocking thread.
///
/// Dropping the instance while a call is still inside it is a use-after-free. A call that outlives
/// the drain window keeps the old copy alive in the background; the library stays mapped either way.
async fn retire(previous: Arc<LoadedPlugin>, name: &str) {
    let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
    while Arc::strong_count(&previous) > 1 {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                plugin = name,
                calls = Arc::strong_count(&previous) - 1,
                "reload drain timed out; retiring the old copy in the background"
            );
            tokio::task::spawn_blocking(move || drop(previous));
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = tokio::task::spawn_blocking(move || drop(previous)).await;
}

async fn discover(
    config: &PluginConfig,
    root: &Path,
    downloader: &reqwest::Client,
    client: &reqwest::Client,
) -> EntryState {
    if let Err(error) = download::ensure(config, root, downloader).await {
        tracing::error!(plugin = %config.name, %error, "plugin download failed");
        return EntryState::Failed { reason: error.to_string() };
    }
    let staged = match stage::stage(config, root).await {
        Ok(staged) => staged,
        Err(error) => {
            tracing::error!(plugin = %config.name, %error, "plugin staging failed");
            return EntryState::Failed { reason: error.to_string() };
        }
    };
    if let Startup::Quarantined { since } = guard::check(root, &config.name, &staged.hash).await {
        tracing::warn!(
            plugin = %config.name,
            since,
            "plugin is quarantined after a previous crash; delete its guard marker to retry"
        );
        return EntryState::Quarantined { since };
    }
    if let Err(error) = guard::arm(root, &config.name, &staged.hash).await {
        tracing::error!(plugin = %config.name, %error, "plugin guard marker failed");
        return EntryState::Failed { reason: error.to_string() };
    }
    match load_staged(config, &staged.path, client.clone()).await {
        Ok(plugin) => {
            tracing::info!(plugin = %config.name, version = %plugin.manifest().version, "plugin loaded");
            guard::schedule_disarm(root.to_owned(), config.name.clone(), staged.hash.clone());
            EntryState::Loaded {
                slot: Arc::new(PluginSlot::new(plugin)),
                hash: staged.hash,
                reason: None,
            }
        }
        Err(reason) => {
            guard::disarm(root, &config.name).await;
            tracing::error!(plugin = %config.name, %reason, "plugin failed to load");
            EntryState::Failed { reason }
        }
    }
}

/// Routing entries are registered once at startup, so a reload may not move them under us.
fn validate_reload(name: &str, previous: &Manifest, next: &Manifest) -> Result<(), String> {
    if previous.sources.len() != next.sources.len() {
        return Err(format!(
            "plugin {name:?} changes its source set; restart the server to register the new sources"
        ));
    }
    for source in &previous.sources {
        let Some(replacement) = next.sources.iter().find(|candidate| candidate.name == source.name)
        else {
            return Err(format!(
                "plugin {name:?} drops source {:?}; restart the server to register the new sources",
                source.name
            ));
        };
        if set(&replacement.prefixes) != set(&source.prefixes)
            || set(&replacement.hosts) != set(&source.hosts)
        {
            return Err(format!(
                "plugin {name:?} source {:?} changes its claims; restart the server to register them",
                source.name
            ));
        }
    }
    Ok(())
}

fn set(values: &[String]) -> BTreeSet<&str> {
    values.iter().map(String::as_str).collect()
}

#[cfg(test)]
#[path = "../tests/plugins_manager.rs"]
mod tests;
