use super::*;

#[test]
fn final_voice_close_uses_the_canonical_event_shape() {
    let message = websocket_closed_message(
        GuildId::new(42),
        &voice::Disconnect { code: 4014, reason: String::from("disconnected"), by_remote: true },
    )
    .expect("voice-close message");
    let value: serde_json::Value = serde_json::from_str(&message).expect("voice-close JSON");
    assert_eq!(
        value,
        serde_json::json!({
            "op": "event",
            "type": "WebSocketClosedEvent",
            "guildId": "42",
            "code": 4014,
            "reason": "disconnected",
            "byRemote": true,
        })
    );
}

#[test]
fn voice_lifecycle_logs_identify_the_connection_without_credentials() {
    let capture = crate::test_support::LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(crate::test_support::capture_subscriber(&capture));

    log_voice_connected("session", GuildId::new(42), 7);
    log_voice_closed(
        "session",
        GuildId::new(42),
        &voice::Disconnect { code: 4014, reason: String::from("disconnected"), by_remote: true },
    );

    let text = capture.text();
    assert!(text.contains("voice connection established"), "{text}");
    assert!(text.contains("voice connection closed"), "{text}");
    assert!(text.contains("session_id=\"session\""), "{text}");
    assert!(text.contains("guild_id=42"), "{text}");
    assert!(text.contains("channel=7"), "{text}");
    assert!(text.contains("code=4014"), "{text}");
    assert!(text.contains("reason=disconnected"), "{text}");
    assert!(text.contains("by_remote=true"), "{text}");
    assert!(!text.contains("token"), "{text}");
}
