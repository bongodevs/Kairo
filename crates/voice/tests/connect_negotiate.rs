use super::*;

#[tokio::test(start_paused = true)]
async fn setup_work_keeps_the_gateway_heartbeat_alive() {
    let (keep_open, queue) = mpsc::channel(1);
    let opening = vec![
        text(r#"{"op":8,"d":{"heartbeat_interval":1000}}"#),
        text(
            r#"{"op":2,"seq":1,"d":{"ip":"127.0.0.1","port":50002,"ssrc":7,"modes":["aead_xchacha20_poly1305_rtpsize"]}}"#,
        ),
    ];
    let target = target();
    let mut gateway = Gateway::establish(
        LiveSocket::new(opening, queue),
        &target,
        Intro::Identify,
        VoiceConfig::default(),
    )
    .await
    .expect("handshake");
    gateway
        .drive_while(async {
            tokio::time::sleep(Duration::from_millis(2_500)).await;
            Ok::<_, ConnectionError>(())
        })
        .await
        .expect("setup");
    drop(keep_open);
    let (socket, _, pending) = gateway.split();
    assert!(pending.is_empty());
    let heartbeat_count = socket
        .sent
        .iter()
        .filter(|frame| serde_json::from_str::<Value>(frame).unwrap()["op"] == 3)
        .count();
    assert_eq!(heartbeat_count, 2, "heartbeats continue during setup");
}

#[tokio::test]
async fn deafened_config_disables_incoming_media() {
    let config = VoiceConfig { deafened: true, ..VoiceConfig::default() };
    let target = target();
    let gateway =
        Gateway::establish(MockSocket::new(hello_frames()), &target, Intro::Identify, config)
            .await
            .expect("handshake");
    let (socket, _, _) = gateway.split();
    let sink = socket.sent_json_of(15).expect("sink wants");
    assert_eq!(sink["d"]["any"], 0);
}

#[tokio::test]
async fn select_protocol_announces_the_discovered_path() {
    let connection = attached(vec![]).await;
    assert_eq!(
        connection.socket.sent.len(),
        6,
        "identify, sink-wants, backend-version, select, speaking-zero, video"
    );
    let sink = connection.socket.json(1);
    assert_eq!(sink["op"], 15);
    assert_eq!(sink["d"]["any"], 100);
    let frame = connection.socket.json(3);
    assert_eq!(frame["op"], 1);
    let version = connection.socket.json(2);
    assert_eq!(version["op"], 16, "READY is answered by a version ask");
    assert_eq!(version["d"], serde_json::json!({}));
    let initial_speaking = connection.socket.json(4);
    assert_eq!(initial_speaking["op"], 5);
    assert_eq!(initial_speaking["d"]["speaking"], 0);
    assert_eq!(initial_speaking["d"]["ssrc"], 7);
    let video = connection.socket.json(5);
    assert_eq!(video["op"], 12, "the stream registration the server reads us from");
    assert_eq!(video["d"]["audio_ssrc"], 7);
    assert_eq!(video["d"]["video_ssrc"], 0);
    assert_eq!(video["d"]["rtx_ssrc"], 0);
    assert_eq!(frame["d"]["protocol"], "udp");
    assert_eq!(frame["d"]["data"]["address"], "1.2.3.4");
    assert_eq!(frame["d"]["data"]["port"], 5004);
    assert_eq!(frame["d"]["data"]["mode"], XCHACHA_MODE);
    assert_eq!(frame["d"]["address"], "1.2.3.4", "duplicated for old servers");
    assert_eq!(frame["d"]["port"], 5004);
    assert_eq!(frame["d"]["mode"], XCHACHA_MODE);
    assert_eq!(frame["d"]["codecs"][0]["name"], "opus");
    assert_eq!(frame["d"]["codecs"][0]["payload_type"], 120);
    assert_eq!(frame["d"]["codecs"][0]["type"], "audio");
    assert_eq!(frame["d"]["codecs"][0]["priority"], u64::from(OPUS_PRIORITY));
    assert_eq!(connection.ssrc(), 7);
    assert_eq!(connection.mode(), XCHACHA_MODE);
    assert_eq!(connection.local(), "1.2.3.4:5004".parse().unwrap());
    assert_eq!(connection.phase(), Phase::AwaitingSession);
    assert!(!connection.ready_to_send(), "nothing is sendable without a key");

    let id = frame["d"]["rtc_connection_id"].as_str().unwrap();
    let parts: Vec<&str> = id.split('-').collect();
    assert_eq!(
        parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
        [8, 4, 4, 4, 12],
        "{id} is not a uuid"
    );
    assert_eq!(&id[14..15], "4", "version 4");
    assert!("89ab".contains(&id[19..20]), "variant bits");
}

#[tokio::test]
async fn session_description_unlocks_transport_only_sending() {
    let description = session_description(0, &[7u8; KEY_LEN], XCHACHA_MODE);
    let mut connection =
        attached(vec![text(&description), Inbound::Closed(1000, "done".to_owned())]).await;
    let closing = connection.run().await.expect("pump runs");
    assert_eq!(closing, Disconnect::remote(1000, "done"));
    assert_eq!(connection.phase(), Phase::Ready);
    assert!(connection.ready_to_send(), "a key is enough without E2EE");
    assert_eq!(connection.socket.sent.len(), 6, "op 4 needs no gateway reply");
}

#[tokio::test]
async fn e2ee_waits_for_a_usable_dave_session() {
    let description = session_description(1, &[7u8; KEY_LEN], XCHACHA_MODE);
    let mut connection = attached(vec![text(&description)]).await;
    let code = connection.run().await.expect("pump ends on EOF").code;
    assert_eq!(code, 1006);
    assert_eq!(connection.phase(), Phase::Ready, "the key did arrive");
    assert!(!connection.ready_to_send(), "but version 1 has no group keys yet");
}

#[tokio::test]
async fn a_negotiated_mode_other_than_ours_is_fatal() {
    let description = session_description(0, &[7u8; KEY_LEN], "aead_aes256_gcm_rtpsize");
    let mut connection = attached(vec![text(&description)]).await;
    let err = connection.run().await.expect_err("mode mismatch stops us");
    assert!(matches!(err, ConnectionError::ModeMismatch { .. }), "{err}");
    assert_eq!(connection.phase(), Phase::AwaitingSession);
    assert_eq!(connection.socket.sent.len(), 6, "a bad description is not answered");
}

#[tokio::test]
async fn a_description_without_a_usable_key_is_rejected() {
    let good = serde_json::to_string(&[7u8; KEY_LEN]).expect("key serializes");
    let bodies = [
        r#"{"op":4,"d":{}}"#.to_owned(),
        format!(r#"{{"op":4,"d":{{"mode":"{XCHACHA_MODE}"}}}}"#),
        format!(r#"{{"op":4,"d":{{"mode":"{XCHACHA_MODE}","secret_key":"base64"}}}}"#),
        format!(r#"{{"op":4,"d":{{"mode":"{XCHACHA_MODE}","secret_key":[300]}}}}"#),
        format!(r#"{{"op":4,"d":{{"mode":"{XCHACHA_MODE}","secret_key":[1,2,3]}}}}"#),
        format!(
            r#"{{"op":4,"d":{{"mode":"{XCHACHA_MODE}","secret_key":{good},"audio_codec":"pcm"}}}}"#
        ),
    ];
    for body in bodies {
        let mut connection = attached(vec![text(&body)]).await;
        let err = connection.run().await.expect_err("bad description stops us");
        assert!(matches!(err, ConnectionError::SessionDescription(_)), "{body} -> {err}");
        assert_eq!(connection.phase(), Phase::AwaitingSession, "{body}");
        assert_eq!(connection.socket.sent.len(), 6, "{body} answered nothing");
    }
}

#[tokio::test]
async fn dave_transitions_are_answered_on_the_gateway() {
    // op 21 prepare-transition: the only answer we owe is op 23 transition-ready.
    let mut connection =
        attached(vec![text(r#"{"op":21,"seq":3,"d":{"transition_id":4,"protocol_version":1}}"#)])
            .await;
    let code = connection.run().await.expect("pump ends on EOF").code;
    assert_eq!(code, 1006);
    let ready = connection.socket.sent_json_of(23).expect("transition-ready sent back");
    assert_eq!(ready["d"]["transition_id"], 4);
    assert_eq!(
        connection.socket.sent.len(),
        7,
        "identify, sink-wants, backend-version, select, speaking-zero, video, transition-ready"
    );
}

#[tokio::test]
async fn binary_dave_frames_reach_the_handler_untouched() {
    // op 25 external-sender with garbage: logged and dropped, the connection keeps running.
    let mut connection = attached(vec![Inbound::Binary(vec![0x00, 0x03, 25, 0])]).await;
    let code = connection.run().await.expect("pump ends on EOF").code;
    assert_eq!(code, 1006);
    assert!(connection.socket.sent_binary.is_empty(), "nothing to answer with");
}

#[test]
fn mode_is_chosen_before_any_key_exists() {
    let mut ready = Ready { ip: "127.0.0.1".into(), port: 50002, ssrc: 7, modes: Vec::new() };
    assert!(matches!(negotiated_mode(&ready), Err(ConnectionError::NoMode)));
    ready.modes = vec!["old_mode".into(), XCHACHA_MODE.into()];
    assert_eq!(negotiated_mode(&ready).unwrap(), XCHACHA_MODE);
}

#[test]
fn ready_address_resolves_with_and_without_a_port() {
    let mut ready =
        Ready { ip: "203.0.113.9:1234".into(), port: 50002, ssrc: 7, modes: Vec::new() };
    assert_eq!(voice_server(&ready).unwrap(), "203.0.113.9:1234".parse().unwrap());
    ready.ip = "203.0.113.9".into();
    assert_eq!(voice_server(&ready).unwrap(), "203.0.113.9:50002".parse().unwrap());
    // A v6 voice host: the port rides in brackets, and a bare address has colons of its own.
    ready.ip = "[2001:db8::9]:1234".into();
    assert_eq!(voice_server(&ready).unwrap(), "[2001:db8::9]:1234".parse().unwrap());
    ready.ip = "2001:db8::9".into();
    assert_eq!(voice_server(&ready).unwrap(), "[2001:db8::9]:50002".parse().unwrap());
    for bad in ["not-an-address", "203.0.113.9:70000", "2001:db8::9]"] {
        ready.ip = bad.into();
        assert!(matches!(voice_server(&ready), Err(ConnectionError::Endpoint(_))), "{bad}");
    }
}

#[test]
fn connection_ids_are_unique_v4_uuids() {
    let first = connection_id().unwrap();
    let second = connection_id().unwrap();
    assert_ne!(first, second);
    assert_eq!(first.len(), 36);
}
