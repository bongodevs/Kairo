use super::*;

#[test]
fn absent_header_is_unauthorized() {
    assert_eq!(authorize(None, Some("pass")), Authz::Unauthorized);
    // Unset password still demands a header; a missing one is 401, not 403.
    assert_eq!(authorize(None, None), Authz::Unauthorized);
}

#[test]
fn matching_token_is_allowed() {
    assert_eq!(authorize(Some("you shall not pass"), Some("you shall not pass")), Authz::Allow);
}

#[test]
fn wrong_token_is_forbidden() {
    assert_eq!(authorize(Some("nope"), Some("pass")), Authz::Forbidden);
}

#[test]
fn any_token_fails_when_no_password_configured() {
    assert_eq!(authorize(Some("anything"), None), Authz::Forbidden);
}

#[test]
fn comparison_is_length_safe_and_exact() {
    assert!(secret_eq(b"abcd", b"abcd"));
    assert!(!secret_eq(b"abcd", b"abce"));
    assert!(!secret_eq(b"abcd", b"abc"));
    assert!(!secret_eq(b"", b"a"));
    assert!(secret_eq(b"", b""));
}
