//! The `Client-Name` contract: a missing header is accepted and logged, with the remote address,
//! exactly like the reference. Nothing validates `NAME/VERSION`.

use super::*;
use crate::test_support::{LogCapture, capture_subscriber};

#[test]
fn a_missing_client_name_warns_with_the_connection_details() {
    let capture = LogCapture::default();
    let _guard = tracing::subscriber::set_default(capture_subscriber(&capture));

    let remote = IpAddr::from([127, 0, 0, 1]);
    log_established("session", 42, remote, false, None, Some("my-lib/1.2"));
    log_established("session", 42, remote, false, None, None);
    log_established("session", 42, remote, false, Some("my-lib/1.2"), Some("my-lib/1.2"));

    let text = capture.text();
    assert_eq!(
        text.matches("library developers: specify a `Client-Name` header").count(),
        2,
        "{text}"
    );
    assert!(text.contains("remote=127.0.0.1"), "{text}");
    assert!(text.contains("agent=my-lib/1.2"), "{text}");
    assert!(text.contains("client_name=\"\""), "{text}");
    assert!(text.contains("client_name=\"my-lib/1.2\""), "{text}");
}
