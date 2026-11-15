use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn root(name: &str) -> PathBuf {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kairo-{name}-{}-{sequence}", std::process::id()))
}

#[tokio::test]
async fn the_marked_build_is_quarantined_and_a_new_build_is_not() {
    let root = root("guard-quarantine");
    tokio::fs::create_dir_all(&root).await.expect("guard root");
    arm(&root, "sources", "first").await.expect("arm");

    let Startup::Quarantined { since } = check(&root, "sources", "first").await else {
        panic!("the marked build must be quarantined");
    };
    assert!(since > 0, "the marker records when the build was tried");
    assert_eq!(check(&root, "sources", "second").await, Startup::Fresh);

    disarm(&root, "sources").await;
    assert_eq!(check(&root, "sources", "first").await, Startup::Fresh);
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn an_unreadable_marker_is_ignored() {
    let root = root("guard-unreadable");
    tokio::fs::create_dir_all(root.join(".guard")).await.expect("guard directory");
    tokio::fs::write(root.join(".guard/sources"), b"not json").await.expect("marker");

    assert_eq!(check(&root, "sources", "first").await, Startup::Fresh);
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}

#[tokio::test]
async fn disarming_only_removes_the_matching_build() {
    let root = root("guard-disarm");
    tokio::fs::create_dir_all(&root).await.expect("guard root");
    arm(&root, "sources", "first").await.expect("arm");

    disarm_if(&root, "sources", "second").await;
    assert!(matches!(check(&root, "sources", "first").await, Startup::Quarantined { .. }));
    disarm_if(&root, "sources", "first").await;
    assert_eq!(check(&root, "sources", "first").await, Startup::Fresh);
    tokio::fs::remove_dir_all(root).await.expect("cleanup");
}
