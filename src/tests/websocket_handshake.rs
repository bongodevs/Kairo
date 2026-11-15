//! The handshake contract: guards, session ids, `ready`, and resume on reconnect.

use super::*;
use crate::players::GuildId;
#[test]
fn handshake_authorization() {
    // A password must be presented and match.
    assert!(authorize_handshake(Some("letmein"), Some("letmein")));
    assert!(!authorize_handshake(Some("wrong"), Some("letmein")));
    assert!(!authorize_handshake(None, Some("letmein")));
    // Unconfigured server: no header is admitted, a header is not.
    assert!(authorize_handshake(None, None));
    assert!(!authorize_handshake(Some("anything"), None));
}

#[test]
fn user_id_must_be_nonzero_numeric() {
    assert!(is_valid_user_id(Some("170939974227541168")));
    assert!(!is_valid_user_id(Some("-5")), "Discord ids are unsigned; never normalize a sign");
    assert!(!is_valid_user_id(Some("0")));
    assert!(!is_valid_user_id(Some("")));
    assert!(!is_valid_user_id(Some("abc")));
    assert!(!is_valid_user_id(None));
}

#[test]
fn session_id_is_sixteen_alphanumeric_chars() {
    let id = generate_session_id().unwrap();
    assert_eq!(id.len(), SESSION_ID_LEN);
    assert!(id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    // Successive ids are distinct (probabilistic, but 36^16 makes a repeat impossible here).
    assert_ne!(id, generate_session_id().unwrap());
}

#[test]
fn ready_frame_is_exact_json() {
    assert_eq!(
        ready_frame("abc123", false).as_str(),
        "{\"op\":\"ready\",\"resumed\":false,\"sessionId\":\"abc123\"}"
    );
    assert_eq!(
        ready_frame("abc123", true).as_str(),
        "{\"op\":\"ready\",\"resumed\":true,\"sessionId\":\"abc123\"}"
    );
}

#[tokio::test]
async fn wrong_password_is_unauthorized() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let err =
        connect_async(request(addr, Some("nope"), Some("123"))).await.expect_err("should reject");
    assert_eq!(rejected_status(err), HttpStatus::UNAUTHORIZED);
}

#[tokio::test]
async fn missing_user_id_is_bad_request() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let err = connect_async(request(addr, Some("letmein"), None)).await.expect_err("should reject");
    assert_eq!(rejected_status(err), HttpStatus::BAD_REQUEST);
}

#[tokio::test]
async fn negative_user_id_is_bad_request() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let err =
        connect_async(request(addr, Some("letmein"), Some("-5"))).await.expect_err("should reject");
    assert_eq!(rejected_status(err), HttpStatus::BAD_REQUEST);
}

#[tokio::test]
async fn zero_user_id_is_bad_request() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let err =
        connect_async(request(addr, Some("letmein"), Some("0"))).await.expect_err("should reject");
    assert_eq!(rejected_status(err), HttpStatus::BAD_REQUEST);
}

#[tokio::test]
async fn handshake_carries_session_resumed_false() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let req = request(addr, Some("letmein"), Some("123"));
    let (_stream, response) = connect_async(req).await.expect("handshake");
    let resumed = response.headers().get("Session-Resumed").and_then(|v| v.to_str().ok());
    assert_eq!(resumed, Some("false"));
    assert_eq!(
        response.headers().get("Lavalink-Api-Version").and_then(|v| v.to_str().ok()),
        Some("4")
    );
}

#[tokio::test]
async fn no_password_admits_a_headerless_client() {
    let (addr, _store) = spawn(None).await;
    let mut stream = connect_async(request(addr, None, Some("123"))).await.expect("handshake").0;
    let frame = stream.next().await.expect("a frame").expect("open");
    assert!(frame.into_text().unwrap().as_str().contains("\"op\":\"ready\""));

    // With a password absent, presenting any Authorization is rejected.
    let err =
        connect_async(request(addr, Some("x"), Some("123"))).await.expect_err("should reject");
    assert_eq!(rejected_status(err), HttpStatus::UNAUTHORIZED);
}

#[tokio::test]
async fn reconnect_resumes_a_parked_session_and_reuses_its_id() {
    let (addr, store) = spawn(Some("letmein")).await;
    // Park a resumable session under a known id, as a client would after PATCH + disconnect.
    let id = "resume1234567890";
    store.reserve(id);
    store
        .create_player(
            id,
            GuildId::new(42),
            player::source::Sources::new(),
            Arc::new(player::track::codec::TrackCodecs::new()),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    store.update(id, Some(true), Some(60));
    store.close(id); // open -> paused
    assert_eq!(store.paused(), 1);

    let mut req = request(addr, Some("letmein"), Some("123"));
    req.headers_mut().insert(HeaderName::from_static("session-id"), HeaderValue::from_static(id));
    let (mut stream, response) = connect_async(req).await.expect("handshake");
    assert_eq!(
        response.headers().get("Session-Resumed").and_then(|v| v.to_str().ok()),
        Some("true"),
        "a matching paused id resumes"
    );

    let ready: serde_json::Value = serde_json::from_str(
        stream.next().await.expect("a frame").expect("open").into_text().unwrap().as_str(),
    )
    .unwrap();
    assert_eq!(ready["op"], "ready");
    assert_eq!(ready["resumed"], true);
    assert_eq!(ready["sessionId"], id, "the id is reused, not regenerated");
    assert_eq!(store.live(), 1, "the resumed session is open again");

    let update: serde_json::Value = serde_json::from_str(
        stream.next().await.expect("player update").expect("open").into_text().unwrap().as_str(),
    )
    .unwrap();
    assert_eq!(update["op"], "playerUpdate");
    assert_eq!(update["guildId"], "42");
    assert_eq!(update["state"]["position"], 0);
    assert_eq!(update["state"]["connected"], false);
    assert_eq!(update["state"]["ping"], -1);

    let stats: serde_json::Value = serde_json::from_str(
        stream.next().await.expect("stats").expect("open").into_text().unwrap().as_str(),
    )
    .unwrap();
    assert_eq!(stats["op"], "stats");

    // Disconnecting re-parks it, because it is still resumable.
    drop(stream);
    for _ in 0..100 {
        if store.live() == 0 && store.paused() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.live(), 0);
    assert_eq!(store.paused(), 1, "a resumed resumable session re-pauses on close");
}

#[tokio::test]
async fn an_unknown_session_id_falls_back_to_a_fresh_session() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let mut req = request(addr, Some("letmein"), Some("123"));
    req.headers_mut().insert(
        HeaderName::from_static("session-id"),
        HeaderValue::from_static("not-a-parked-session"),
    );
    let (mut stream, response) = connect_async(req).await.expect("handshake");
    assert_eq!(
        response.headers().get("Session-Resumed").and_then(|v| v.to_str().ok()),
        Some("false"),
        "an unknown id does not resume"
    );

    let ready: serde_json::Value = serde_json::from_str(
        stream.next().await.expect("a frame").expect("open").into_text().unwrap().as_str(),
    )
    .unwrap();
    assert_eq!(ready["resumed"], false);
    assert_eq!(
        ready["sessionId"].as_str().unwrap().len(),
        SESSION_ID_LEN,
        "a fresh id is minted instead"
    );
}

#[tokio::test]
async fn the_same_user_can_hold_several_independent_sessions() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut first = connect(addr).await;
    let mut second = connect(addr).await;
    let first_ready = next_json(&mut first).await;
    let second_ready = next_json(&mut second).await;

    assert_ne!(first_ready["sessionId"], second_ready["sessionId"]);
    assert_eq!(store.live(), 2);
}

#[tokio::test]
async fn a_live_session_id_does_not_replace_its_connection() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut first = connect(addr).await;
    let first_ready = next_json(&mut first).await;
    let first_id = first_ready["sessionId"].as_str().expect("session id").to_owned();

    let mut req = request(addr, Some("letmein"), Some("170939974227541168"));
    req.headers_mut().insert(
        HeaderName::from_static("session-id"),
        HeaderValue::from_str(&first_id).expect("session id header"),
    );
    let (mut second, response) = connect_async(req).await.expect("second handshake");
    let second_ready = next_json(&mut second).await;

    assert_eq!(
        response.headers().get("Session-Resumed").and_then(|value| value.to_str().ok()),
        Some("false")
    );
    assert_eq!(second_ready["resumed"], false);
    assert_ne!(second_ready["sessionId"], first_id);
    assert_eq!(store.live(), 2);
}

#[tokio::test]
async fn an_expired_session_id_starts_a_fresh_session() {
    let (addr, store) = spawn(Some("letmein")).await;
    let expired = "expired123456789";
    assert!(store.reserve(expired));
    assert_eq!(store.update(expired, Some(true), Some(0)), Some((true, 0)));
    store.close(expired);
    for _ in 0..100 {
        if store.paused() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(store.paused(), 0);

    let mut req = request(addr, Some("letmein"), Some("123"));
    req.headers_mut()
        .insert(HeaderName::from_static("session-id"), HeaderValue::from_static(expired));
    let (mut stream, response) = connect_async(req).await.expect("fresh handshake");
    let ready = next_json(&mut stream).await;

    assert_eq!(
        response.headers().get("Session-Resumed").and_then(|value| value.to_str().ok()),
        Some("false")
    );
    assert_eq!(ready["resumed"], false);
    assert_ne!(ready["sessionId"], expired);
}
