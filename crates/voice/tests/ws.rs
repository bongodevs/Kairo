use tokio_tungstenite::tungstenite::http::header::{CONNECTION, HOST, SEC_WEBSOCKET_KEY, UPGRADE};

use super::*;
#[test]
fn endpoint_forms_all_reach_the_tls_gateway() {
    for endpoint in ["voice.example.com", "wss://voice.example.com", "voice.example.com:443"] {
        let url = gateway_url(endpoint, false).expect(endpoint);
        if endpoint.ends_with(":443") {
            assert_eq!(url, "wss://voice.example.com:443/?v=8");
        } else {
            assert_eq!(url, "wss://voice.example.com/?v=8", "from {endpoint}");
        }
    }
}

#[test]
fn gateway_request_keeps_the_websocket_upgrade_headers() {
    let request = gateway_request("voice.example.com", false).expect("request");
    for header in [HOST, CONNECTION, UPGRADE, SEC_WEBSOCKET_KEY] {
        assert!(request.headers().contains_key(&header), "missing {header}");
    }
    assert_eq!(
        request.headers()[USER_AGENT],
        HeaderValue::from_str(&user_agent()).expect("user agent")
    );
}

#[test]
fn endpoint_normalization_ignores_the_supplied_scheme() {
    assert_eq!(
        gateway_url("ws://voice.example.com/path?old=true", false).expect("url"),
        "wss://voice.example.com/?v=8"
    );
}

#[test]
fn the_test_scheme_is_plaintext_only_when_the_test_feature_is_on() {
    let url = gateway_url("plaintext://127.0.0.1:1234/track?old=true", false).expect("url");
    if cfg!(feature = "plaintext-gateway") {
        assert_eq!(url, "ws://127.0.0.1:1234/?v=8");
    } else {
        assert_eq!(url, "wss://127.0.0.1:1234/?v=8");
    }
}

#[test]
fn port_80_override_is_explicit() {
    assert_eq!(
        gateway_url("voice.example.com:80", false).expect("url"),
        "wss://voice.example.com:80/?v=8"
    );
    assert_eq!(
        gateway_url("voice.example.com:80", true).expect("url"),
        "wss://voice.example.com/?v=8"
    );
}

#[test]
fn empty_endpoint_is_rejected() {
    assert!(matches!(gateway_url("  ", false), Err(GatewayError::Connect(_))));
}

#[test]
fn a_long_close_reason_is_cut_where_the_wire_can_still_carry_it() {
    assert_eq!(within_close_limit("done".into()), "done");
    let long = within_close_limit("x".repeat(400));
    assert_eq!(long.len(), CLOSE_REASON_LIMIT);
    let multibyte = within_close_limit("é".repeat(200));
    assert!(multibyte.len() <= CLOSE_REASON_LIMIT);
    assert!(multibyte.is_char_boundary(multibyte.len()));
    assert_eq!(multibyte, "é".repeat(CLOSE_REASON_LIMIT / 2));
}
