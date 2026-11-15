use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn root(name: &str) -> PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kairo-download-{name}-{}-{sequence}", std::process::id()))
}

fn config(repo: Option<&str>, tag: Option<&str>) -> PluginConfig {
    PluginConfig {
        name: String::from("plugin"),
        repo: repo.map(str::to_owned),
        tag: tag.map(str::to_owned),
        config: toml::Table::new(),
    }
}

struct Server {
    origin: String,
    hits: Arc<AtomicUsize>,
}

async fn serve(body: Vec<u8>, sidecar: Option<String>) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::Relaxed);
            let mut request = [0; 2048];
            let read = stream.read(&mut request).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let (status, body) = if path.ends_with(".sha256") {
                match &sidecar {
                    Some(text) => ("200 OK", text.clone().into_bytes()),
                    None => ("404 Not Found", Vec::new()),
                }
            } else {
                ("200 OK", body.clone())
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        }
    });
    Server { origin: format!("http://{address}"), hits }
}

fn lock_entry(bytes: &[u8]) -> LockEntry {
    LockEntry {
        name: String::from("plugin"),
        tag: String::from("v1"),
        asset: asset_name("plugin").expect("asset"),
        sha256: hex(&Sha256::digest(bytes)),
    }
}

#[tokio::test]
async fn a_missing_plugin_is_downloaded_and_locked() {
    let bytes = b"plugin bytes".to_vec();
    let digest = hex(&Sha256::digest(&bytes));
    let server = serve(bytes.clone(), Some(format!("{digest}  plugin.kai\n"))).await;
    let root = root("fresh");
    let config = config(Some("owner/plugin"), Some("v1"));
    let client = client().expect("client");

    ensure_from(&config, &root, &client, &server.origin).await.expect("download");

    let installed = tokio::fs::read(config.install_path(&root)).await.expect("installed plugin");
    assert_eq!(installed, bytes);
    assert_eq!(server.hits.load(Ordering::Relaxed), 2);
    let lock = read_lock(&root.join(LOCK_FILE)).await.expect("lock");
    assert_eq!(lock.entries, [lock_entry(&bytes)]);
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn a_matching_lock_and_file_skip_the_network() {
    let bytes = b"plugin bytes".to_vec();
    let server = serve(bytes.clone(), None).await;
    let root = root("cached");
    tokio::fs::create_dir_all(&root).await.expect("root");
    let config = config(Some("owner/plugin"), Some("v1"));
    tokio::fs::write(config.install_path(&root), &bytes).await.expect("installed plugin");
    write_lock(&root.join(LOCK_FILE), &Lock { entries: vec![lock_entry(&bytes)] })
        .await
        .expect("lock");
    let client = client().expect("client");

    ensure_from(&config, &root, &client, &server.origin).await.expect("cached plugin");

    assert_eq!(server.hits.load(Ordering::Relaxed), 0);
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn a_file_that_does_not_match_the_lock_is_refused() {
    let bytes = b"plugin bytes".to_vec();
    let server = serve(bytes.clone(), None).await;
    let root = root("tampered");
    tokio::fs::create_dir_all(&root).await.expect("root");
    let config = config(Some("owner/plugin"), Some("v1"));
    tokio::fs::write(config.install_path(&root), b"different bytes")
        .await
        .expect("installed plugin");
    write_lock(&root.join(LOCK_FILE), &Lock { entries: vec![lock_entry(&bytes)] })
        .await
        .expect("lock");
    let client = client().expect("client");

    let error =
        ensure_from(&config, &root, &client, &server.origin).await.expect_err("hash mismatch");

    assert!(matches!(error, DownloadError::LockedHashMismatch));
    assert_eq!(server.hits.load(Ordering::Relaxed), 0);
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn a_published_sha256_must_match_the_asset() {
    let bytes = b"plugin bytes".to_vec();
    let server = serve(bytes.clone(), Some(format!("{}  plugin.kai\n", "0".repeat(64)))).await;
    let root = root("sidecar");
    let config = config(Some("owner/plugin"), Some("v1"));
    let client = client().expect("client");

    let error =
        ensure_from(&config, &root, &client, &server.origin).await.expect_err("sidecar mismatch");

    assert!(matches!(error, DownloadError::AssetHashMismatch));
    assert!(!config.install_path(&root).exists());
    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn a_local_plugin_never_touches_the_network() {
    let root = root("local");
    let client = client().expect("client");

    ensure_from(&config(None, None), &root, &client, "http://127.0.0.1:9")
        .await
        .expect("local plugin");

    assert!(!root.exists());
}

#[test]
fn asset_names_follow_the_release_layout() {
    assert_eq!(asset_for("sources", "linux", "x86_64").expect("linux"), "sources-linux-x86_64.kai");
    assert_eq!(
        asset_for("sources", "macos", "aarch64").expect("macos"),
        "sources-macos-aarch64.kai"
    );
    assert_eq!(
        asset_for("sources", "windows", "x86_64").expect("windows"),
        "sources-windows-x86_64.kai"
    );
    assert!(asset_for("sources", "freebsd", "x86_64").is_err());
    assert!(asset_for("sources", "linux", "riscv64").is_err());
}
