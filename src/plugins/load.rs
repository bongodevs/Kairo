use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use kairo_plugin_api::{HostCallbacks, LoadedPlugin, Manifest};

use super::host_api::PluginHost;
use super::native;
use crate::config::PluginConfig;

pub(super) async fn load_staged(
    config: &PluginConfig,
    path: &Path,
    client: reqwest::Client,
) -> Result<Arc<LoadedPlugin>, String> {
    let init = serde_json::to_value(&config.config).map_err(|error| error.to_string())?;
    let callbacks: Arc<dyn HostCallbacks> = Arc::new(PluginHost::new(config.name.clone(), client));
    let expected_name = config.name.clone();
    let check_name = expected_name.clone();
    let path = path.to_owned();
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

pub(super) fn validate_manifest(expected_name: &str, manifest: &Manifest) -> Result<(), String> {
    if manifest.name != expected_name {
        return Err(format!(
            "plugin manifest name {:?} does not match config name {expected_name:?}",
            manifest.name
        ));
    }
    if manifest.version.trim().is_empty() {
        return Err(String::from("plugin manifest version is empty"));
    }
    let mut sources = HashSet::new();
    for source in &manifest.sources {
        if !super::adapter::valid_label(&source.name) {
            return Err(format!("plugin source name {:?} is invalid", source.name));
        }
        if !sources.insert(source.name.as_str()) {
            return Err(format!("plugin source name {:?} is duplicated", source.name));
        }
    }
    Ok(())
}
