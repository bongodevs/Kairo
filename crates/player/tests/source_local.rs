use std::io::SeekFrom;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

fn test_path(extension: &str) -> std::path::PathBuf {
    let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!("kairo-player-source-{}-{id}.{extension}", std::process::id()))
}

#[tokio::test]
async fn readable_file_opens_with_length_extension_and_seeking() {
    let path = test_path("OGG");
    tokio::fs::write(&path, b"abcdef").await.expect("write file");
    let reference = SourceReference::with_title(path.to_string_lossy(), "track");

    let SourceResult::Opened(mut opened) = LocalSource.load(&reference).await.expect("open local")
    else {
        panic!("expected opened source");
    };

    assert_eq!(opened.source_name(), "local");
    assert_eq!(opened.content_length(), Some(6));
    assert_eq!(opened.hints().file_extension.as_deref(), Some("OGG"));
    assert!(opened.is_seekable());
    opened.seek(SeekFrom::Start(2)).await.expect("seek");
    let mut bytes = [0u8; 2];
    assert_eq!(opened.read(&mut bytes).await.expect("read"), 2);
    assert_eq!(&bytes, b"cd");

    tokio::fs::remove_file(path).await.expect("remove file");
}

#[tokio::test]
async fn missing_paths_and_directories_are_not_matches() {
    let missing = test_path("missing");
    let missing_result = LocalSource
        .load(&SourceReference::new(missing.to_string_lossy()))
        .await
        .expect("missing result");
    assert!(matches!(missing_result, SourceResult::NotMine));

    let directory_result = LocalSource
        .load(&SourceReference::new(std::env::temp_dir().to_string_lossy()))
        .await
        .expect("directory result");
    assert!(matches!(directory_result, SourceResult::NotMine));
}
