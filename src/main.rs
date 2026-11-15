use std::io;
use std::path::Path;

use anyhow::Context;
use kairo::config::{Config, ConfigError};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The voice gateway needs a rustls process-default provider (see Cargo.toml).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let path = config_path();
    let (config, used_defaults) = load_config(&path)?;
    let _logging_guard = kairo::telemetry::init(&config.logging);
    kairo::telemetry::banner::print(&config.logging);
    if !used_defaults {
        tracing::info!(
            path = %path.display(),
            level = %config.logging.level,
            "configuration loaded"
        );
    }
    if let Ok(rust_log) = std::env::var("RUST_LOG")
        && !rust_log.trim().is_empty()
    {
        tracing::warn!(%rust_log, "RUST_LOG overrides the logging level");
    }
    let update_logging = config.logging.clone();
    tokio::spawn(async move {
        kairo::telemetry::update::check(&update_logging).await;
    });
    kairo::serve(&config).await.context("run server")
}

fn config_path() -> std::path::PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return std::path::PathBuf::from(arg);
    }
    if let Ok(path) = std::env::var("KAIRO_CONFIG") {
        return std::path::PathBuf::from(path);
    }
    ["config.local.toml", "config.toml"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("config.toml"))
}

fn load_config(path: &Path) -> anyhow::Result<(Config, bool)> {
    match Config::load(path) {
        Ok(config) => Ok((config, false)),
        Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok((Config::defaults(), true))
        }
        Err(err) => Err(err).with_context(|| format!("load config {}", path.display())),
    }
}
