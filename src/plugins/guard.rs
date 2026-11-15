use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::utils::time::now_millis;

const STABILITY_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    hash: String,
    since: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Startup {
    Fresh,
    Quarantined { since: u64 },
}

#[derive(Debug, thiserror::Error)]
pub(super) enum GuardError {
    #[error("create plugin guard directory {path}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("write plugin guard marker {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("encode plugin guard marker")]
    Encode(#[source] serde_json::Error),
}

/// A marker that still names the build being loaded means the previous process died on it.
pub(super) async fn check(root: &Path, name: &str, hash: &str) -> Startup {
    match read(root, name).await {
        Some(marker) if marker.hash == hash => Startup::Quarantined { since: marker.since },
        _ => Startup::Fresh,
    }
}

pub(super) async fn arm(root: &Path, name: &str, hash: &str) -> Result<(), GuardError> {
    let directory = directory(root);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|source| GuardError::Create { path: directory, source })?;
    let marker = Marker { hash: hash.to_owned(), since: now_millis() };
    let body = serde_json::to_vec(&marker).map_err(GuardError::Encode)?;
    let path = marker_path(root, name);
    tokio::fs::write(&path, body).await.map_err(|source| GuardError::Write { path, source })
}

pub(super) async fn disarm(root: &Path, name: &str) {
    if let Err(error) = tokio::fs::remove_file(marker_path(root, name)).await
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(plugin = name, %error, "failed to remove plugin guard marker");
    }
}

/// Remove the marker only while it still describes `hash`; a newer reload keeps its own marker.
pub(super) async fn disarm_if(root: &Path, name: &str, hash: &str) {
    if read(root, name).await.is_some_and(|marker| marker.hash == hash) {
        disarm(root, name).await;
    }
}

/// The marker stays for one stability window after a successful load, then the build is trusted.
pub(super) fn schedule_disarm(root: PathBuf, name: String, hash: String) {
    tokio::spawn(async move {
        tokio::time::sleep(STABILITY_WINDOW).await;
        disarm_if(&root, &name, &hash).await;
    });
}

fn directory(root: &Path) -> PathBuf {
    root.join(".guard")
}

fn marker_path(root: &Path, name: &str) -> PathBuf {
    directory(root).join(name)
}

async fn read(root: &Path, name: &str) -> Option<Marker> {
    let bytes = tokio::fs::read(marker_path(root, name)).await.ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(marker) => Some(marker),
        Err(error) => {
            tracing::warn!(plugin = name, %error, "plugin guard marker is unreadable; ignoring it");
            None
        }
    }
}

#[cfg(test)]
#[path = "../tests/plugins_guard.rs"]
mod tests;
