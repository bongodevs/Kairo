use super::*;
#[tokio::test]
async fn a_mask_change_announces_op5_once_per_change() {
    let mut connection = attached(vec![]).await;
    let speaking = connection.speaking();
    speaking.set(SPEAKING_MICROPHONE).await.expect("the pump is live");
    speaking.set(SPEAKING_MICROPHONE).await.expect("the pump is live");
    speaking.set(SPEAKING_MICROPHONE | SPEAKING_SOUNDSHARE).await.expect("the pump is live");
    assert_eq!(connection.run().await.expect("pump ends on EOF").code, 1006);
    assert_eq!(
        announced(&connection.socket.sent),
        [0, 1, 3],
        "the mask already in effect sends nothing"
    );

    let frame = connection
        .socket
        .sent
        .iter()
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .find(|frame| frame["op"] == 5 && frame["d"]["speaking"] != 0)
        .expect("a speaking frame");
    assert_eq!(frame["d"]["speaking"], 1);
    assert_eq!(frame["d"]["delay"], 0);
    assert_eq!(frame["d"]["ssrc"], 7, "READY's ssrc, unsigned");
}

#[tokio::test]
async fn going_quiet_is_silent_unless_the_stop_frame_is_asked_for() {
    let mut connection = attached(vec![]).await;
    let speaking = connection.speaking();
    for mask in [SPEAKING_MICROPHONE, 0] {
        speaking.set(mask).await.expect("the pump is live");
    }
    connection.run().await.expect("pump ends on EOF");
    assert_eq!(
        announced(&connection.socket.sent),
        [0, 1],
        "a stop frame makes listeners cut buffered audio"
    );

    let mut connection =
        attached_with(vec![], VoiceConfig { send_speaking_stop: true, ..VoiceConfig::default() })
            .await;
    let speaking = connection.speaking();
    for mask in [SPEAKING_MICROPHONE, 0, 0] {
        speaking.set(mask).await.expect("the pump is live");
    }
    connection.run().await.expect("pump ends on EOF");
    assert_eq!(
        announced(&connection.socket.sent),
        [0, 1, 0],
        "announced once, and the repeat zero is no change"
    );
}

#[tokio::test]
async fn a_late_joiner_gets_the_current_mask_resent() {
    let description = session_description(0, &[7u8; KEY_LEN], XCHACHA_MODE);
    let mut connection = attached(vec![
        text(&description),
        text(r#"{"op":12,"seq":3,"d":{"user_id":"9","audio_ssrc":5}}"#),
        // Someone else's mask is never our business.
        text(r#"{"op":5,"seq":4,"d":{"user_id":"9","speaking":1}}"#),
    ])
    .await;
    let speaking = connection.speaking();
    speaking.set(SPEAKING_MICROPHONE | SPEAKING_PRIORITY).await.expect("the pump is live");
    assert_eq!(connection.run().await.expect("pump ends on EOF").code, 1006);
    assert_eq!(
        announced(&connection.socket.sent),
        [0, 5, 5, 5],
        "own change, resend on op 4, resend on op 12"
    );
    assert_eq!(
        connection.socket.sent.len(),
        9,
        "identify, sink-wants, backend-version, select, initial zero, video and three speaking frames"
    );
}

#[tokio::test]
async fn nothing_is_resent_while_we_are_silent() {
    let description = session_description(0, &[7u8; KEY_LEN], XCHACHA_MODE);
    let mut connection = attached(vec![
        text(&description),
        text(r#"{"op":12,"seq":3,"d":{"user_id":"9","audio_ssrc":5}}"#),
    ])
    .await;
    assert_eq!(connection.run().await.expect("pump ends on EOF").code, 1006);
    assert_eq!(connection.socket.sent.len(), 6, "only the initial zero is announced while idle");
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_makes_the_caller_wait_instead_of_dropping() {
    let connection = attached(vec![]).await; // the pump has not started: nothing drains
    let speaking = connection.speaking();
    for command in 1..=COMMAND_QUEUE as u8 {
        speaking.set(command).await.expect("a slot is free");
    }
    assert!(
        tokio::time::timeout(Duration::ZERO, speaking.set(COMMAND_QUEUE as u8 + 1)).await.is_err(),
        "a waiting caller is the backpressure, not a lost update"
    );
}

#[tokio::test]
async fn a_change_after_the_pump_stopped_is_an_error() {
    let mut connection = attached(vec![]).await;
    let speaking = connection.speaking();
    let frames = connection.frames();
    assert_eq!(connection.run().await.expect("pump ends on EOF").code, 1006);
    assert!(
        matches!(speaking.set(SPEAKING_MICROPHONE).await, Err(ConnectionError::Stopped)),
        "the queue closes with the pump"
    );
    assert!(
        matches!(frames.notify().await, Err(ConnectionError::Stopped)),
        "both handles report the connection stopped"
    );
    assert!(
        matches!(connection.run().await, Err(ConnectionError::AlreadyRun)),
        "a driver cannot take the queue twice"
    );
}
