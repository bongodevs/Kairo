use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::hash;
use crate::config::PluginConfig;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
pub(super) enum StageError {
    #[error("create plugin staging directory {path}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("copy plugin from {source_path} to {target_path}")]
    Copy {
        source_path: PathBuf,
        target_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("hash staged plugin {path}")]
    Hash {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("plugin hashing worker stopped")]
    Worker(#[source] tokio::task::JoinError),
    #[error("publish staged plugin {path}")]
    Publish {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug)]
pub(super) struct Staged {
    pub(super) path: PathBuf,
    pub(super) hash: String,
}

pub(super) async fn stage(config: &PluginConfig, root: &Path) -> Result<Staged, StageError> {
    let source = config.install_path(root);
    let run = root.join(".run");
    tokio::fs::create_dir_all(&run)
        .await
        .map_err(|source| StageError::Create { path: run.clone(), source })?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = run.join(format!("{}-{}-{sequence}.tmp", config.name, std::process::id()));
    if let Err(error) = tokio::fs::copy(&source, &temporary).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(StageError::Copy {
            source_path: source,
            target_path: temporary,
            source: error,
        });
    }
    let hash_path = temporary.clone();
    let digest = match tokio::task::spawn_blocking(move || hash::file(&hash_path)).await {
        Ok(Ok(digest)) => digest,
        Ok(Err(error)) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(StageError::Hash { path: temporary, source: error });
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(StageError::Worker(error));
        }
    };
    let hash = hash::hex(&digest);
    let staged = run.join(format!("{}-{hash}.{}", config.name, kairo_plugin_api::EXTENSION));
    match tokio::fs::try_exists(&staged).await {
        Ok(true) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Ok(Staged { path: staged, hash });
        }
        Ok(false) => {}
        Err(source) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(StageError::Publish { path: staged, source });
        }
    }
    if let Err(source) = tokio::fs::rename(&temporary, &staged).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(StageError::Publish { path: staged, source });
    }
    Ok(Staged { path: staged, hash })
}

#[cfg(test)]
#[path = "../tests/plugins_stage.rs"]
mod tests;
