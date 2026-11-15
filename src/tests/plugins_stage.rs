use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn root(name: &str) -> std::path::PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kairo-{name}-{}-{sequence}", std::process::id()))
}

fn config() -> PluginConfig {
    PluginConfig { name: "hello-source".into(), repo: None, tag: None, config: toml::Table::new() }
}

#[tokio::test]
async fn staged_copy_is_content_addressed_and_reused() {
    let root = root("plugin-stage");
    tokio::fs::create_dir_all(&root).await.expect("plugin root");
    let config = config();
    let installed = config.install_path(&root);
    tokio::fs::write(&installed, b"first build").await.expect("installed plugin");

    let first = stage(&config, &root).await.expect("first stage");
    let same = stage(&config, &root).await.expect("same stage");
    assert_eq!(first, same);
    assert_eq!(tokio::fs::read(&first).await.expect("staged bytes"), b"first build");
    assert!(first.starts_with(root.join(".run")));
    assert!(first.file_name().unwrap().to_string_lossy().starts_with("hello-source-"));
    assert_eq!(first.extension().and_then(|ext| ext.to_str()), Some("kai"));

    tokio::fs::write(&installed, b"second build").await.expect("updated plugin");
    let second = stage(&config, &root).await.expect("second stage");
    assert_ne!(first, second);
    assert_eq!(tokio::fs::read(second).await.expect("updated bytes"), b"second build");
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn missing_installed_library_is_a_typed_copy_error() {
    let root = root("plugin-stage-missing");
    let error = stage(&config(), &root).await.expect_err("missing plugin");

    assert!(matches!(error, StageError::Copy { .. }));
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}
