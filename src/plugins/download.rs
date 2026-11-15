use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::PluginConfig;

const LOCK_FILE: &str = "plugins.lock";
const TEMP_DIR: &str = ".tmp";
const MAX_ASSET_BYTES: u64 = 32 * 1024 * 1024;
const GITHUB: &str = "https://github.com";

#[derive(Debug, thiserror::Error)]
pub(super) enum DownloadError {
    #[error("plugin {name:?} has no release asset for {os}/{arch}")]
    UnsupportedPlatform { name: String, os: String, arch: String },
    #[error("request {url}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("download {url} answered {status}")]
    Status { url: String, status: reqwest::StatusCode },
    #[error("read downloaded plugin")]
    Read(#[source] reqwest::Error),
    #[error("downloaded plugin is larger than {MAX_ASSET_BYTES} bytes")]
    TooLarge,
    #[error("published .sha256 does not match the downloaded plugin")]
    AssetHashMismatch,
    #[error("published .sha256 is not a SHA-256 digest")]
    InvalidSidecar,
    #[error("installed plugin does not match {LOCK_FILE}")]
    LockedHashMismatch,
    #[error("check installed plugin {path}")]
    Check {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("hash installed plugin {path}")]
    Hash {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("hashing worker stopped")]
    HashWorker(#[source] tokio::task::JoinError),
    #[error("read {path}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parse {path}")]
    ParseLock {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serialize {LOCK_FILE}")]
    SerializeLock(#[source] toml::ser::Error),
    #[error("write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct Lock {
    #[serde(default, rename = "plugin")]
    entries: Vec<LockEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct LockEntry {
    name: String,
    tag: String,
    asset: String,
    sha256: String,
}

/// Downloads the configured release asset when the installed file is missing or out of date.
pub(super) async fn ensure(
    config: &PluginConfig,
    root: &Path,
    client: &reqwest::Client,
) -> Result<(), DownloadError> {
    ensure_from(config, root, client, GITHUB).await
}

async fn ensure_from(
    config: &PluginConfig,
    root: &Path,
    client: &reqwest::Client,
    origin: &str,
) -> Result<(), DownloadError> {
    let (Some(repo), Some(tag)) = (&config.repo, &config.tag) else {
        return Ok(());
    };
    let asset = asset_name(&config.name)?;
    let installed = config.install_path(root);
    let lock_path = root.join(LOCK_FILE);
    let mut lock = read_lock(&lock_path).await?;

    if let Some(entry) = lock.entries.iter().find(|entry| entry.name == config.name)
        && entry.tag == *tag
        && entry.asset == asset
    {
        match tokio::fs::try_exists(&installed).await {
            Ok(true) => {
                return match hash_file(&installed).await? == entry.sha256 {
                    true => Ok(()),
                    false => Err(DownloadError::LockedHashMismatch),
                };
            }
            Ok(false) => {}
            Err(source) => return Err(DownloadError::Check { path: installed, source }),
        }
    }

    let (bytes, published) = release_through_url(client, origin, repo, tag, &asset).await?;
    let sha256 = hex(&Sha256::digest(&bytes));
    if published.is_some_and(|published| !published.eq_ignore_ascii_case(&sha256)) {
        return Err(DownloadError::AssetHashMismatch);
    }

    write_plugin(&bytes, &installed, root).await?;
    lock.entries.retain(|entry| entry.name != config.name);
    lock.entries.push(LockEntry { name: config.name.clone(), tag: tag.clone(), asset, sha256 });
    lock.entries.sort_by(|left, right| left.name.cmp(&right.name));
    write_lock(&lock_path, &lock).await
}

async fn release_through_url(
    client: &reqwest::Client,
    origin: &str,
    repo: &str,
    tag: &str,
    asset: &str,
) -> Result<(Vec<u8>, Option<String>), DownloadError> {
    let base = format!("{origin}/{repo}/releases/download/{tag}");
    let url = format!("{base}/{asset}");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|source| DownloadError::Request { url: url.clone(), source })?;
    let bytes = read_body(require_success(url.clone(), response).await?).await?;

    let sidecar = format!("{url}.sha256");
    let response = client
        .get(&sidecar)
        .send()
        .await
        .map_err(|source| DownloadError::Request { url: sidecar.clone(), source })?;
    let published = match response.status() {
        status if status.is_success() => Some(parse_sha(&read_body(response).await?)?),
        reqwest::StatusCode::NOT_FOUND => None,
        status => return Err(DownloadError::Status { url: sidecar, status }),
    };
    Ok((bytes, published))
}

async fn require_success(
    url: String,
    response: reqwest::Response,
) -> Result<reqwest::Response, DownloadError> {
    match response.status().is_success() {
        true => Ok(response),
        false => Err(DownloadError::Status { url, status: response.status() }),
    }
}

async fn read_body(response: reqwest::Response) -> Result<Vec<u8>, DownloadError> {
    if response.content_length().is_some_and(|length| length > MAX_ASSET_BYTES) {
        return Err(DownloadError::TooLarge);
    }
    let bytes = response.bytes().await.map_err(DownloadError::Read)?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(DownloadError::TooLarge);
    }
    Ok(bytes.to_vec())
}

async fn write_plugin(bytes: &[u8], installed: &Path, root: &Path) -> Result<(), DownloadError> {
    let temp_dir = root.join(TEMP_DIR);
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|source| DownloadError::Write { path: temp_dir.clone(), source })?;
    let stem = installed.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let temp = temp_dir.join(format!("{stem}.{}.tmp", std::process::id()));
    tokio::fs::write(&temp, bytes)
        .await
        .map_err(|source| DownloadError::Write { path: temp.clone(), source })?;
    tokio::fs::rename(&temp, installed)
        .await
        .map_err(|source| DownloadError::Write { path: installed.to_path_buf(), source })
}

async fn read_lock(path: &Path) -> Result<Lock, DownloadError> {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Lock::default()),
        Err(source) => return Err(DownloadError::ReadFile { path: path.to_path_buf(), source }),
    };
    toml::from_str(&text)
        .map_err(|source| DownloadError::ParseLock { path: path.to_path_buf(), source })
}

async fn write_lock(path: &Path, lock: &Lock) -> Result<(), DownloadError> {
    let text = toml::to_string(lock).map_err(DownloadError::SerializeLock)?;
    let temp = path.with_extension("lock.tmp");
    tokio::fs::write(&temp, text)
        .await
        .map_err(|source| DownloadError::Write { path: temp.clone(), source })?;
    tokio::fs::rename(&temp, path)
        .await
        .map_err(|source| DownloadError::Write { path: path.to_path_buf(), source })
}

async fn hash_file(path: &Path) -> Result<String, DownloadError> {
    let path = path.to_path_buf();
    let digest = tokio::task::spawn_blocking(move || hash(&path))
        .await
        .map_err(DownloadError::HashWorker)??;
    Ok(hex(&digest))
}

fn hash(path: &Path) -> Result<[u8; 32], DownloadError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|source| DownloadError::Hash { path: path.to_path_buf(), source })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| DownloadError::Hash { path: path.to_path_buf(), source })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn parse_sha(bytes: &[u8]) -> Result<String, DownloadError> {
    let text = String::from_utf8_lossy(bytes);
    text.split_whitespace()
        .next()
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or(DownloadError::InvalidSidecar)
}

fn asset_name(name: &str) -> Result<String, DownloadError> {
    asset_for(name, std::env::consts::OS, std::env::consts::ARCH)
}

fn asset_for(name: &str, os: &str, arch: &str) -> Result<String, DownloadError> {
    let platform = match os {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        other => {
            return Err(DownloadError::UnsupportedPlatform {
                name: name.to_owned(),
                os: other.to_owned(),
                arch: arch.to_owned(),
            });
        }
    };
    let arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => {
            return Err(DownloadError::UnsupportedPlatform {
                name: name.to_owned(),
                os: platform.to_owned(),
                arch: other.to_owned(),
            });
        }
    };
    Ok(format!("{name}-{platform}-{arch}.{}", kairo_plugin_api::EXTENSION))
}

/// Longer than a plugin call: a release asset is a few megabytes, not a JSON response.
pub(super) fn client() -> Result<reqwest::Client, reqwest::Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(kairo_core::http::USER_AGENT)
        .build()
}

#[cfg(test)]
#[path = "../tests/plugins_download.rs"]
mod tests;
