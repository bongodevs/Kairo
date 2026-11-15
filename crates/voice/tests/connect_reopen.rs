use super::*;
/// A leave request the pump has not read yet: the close goes out on the wire before the report,
/// and a close we asked for is reported as ours.
#[tokio::test]
async fn leaving_on_request_closes_the_socket_before_it_reports() {
    let mut connection = attached(vec![]).await;
    let lifecycle = connection.lifecycle();
    lifecycle.disconnect().await.expect("the pump has not started yet");
    let closing = connection.run().await.expect("our close ends the pump");
    assert_eq!(closing, Disconnect::local(1000, ""));
    assert_eq!(
        connection.socket.closed,
        Some((1000, String::new())),
        "the peer is told we left, not left to guess"
    );
    assert!(
        matches!(lifecycle.disconnect().await, Err(ConnectionError::Stopped)),
        "the queue closes with the connection"
    );
}

/// The code a voice host understands as "give me another one": an unguarded run ends on it, so
/// a host that does not supervise still reports a retryable close.
#[tokio::test]
async fn a_reconnect_request_ends_the_run_with_the_code_that_asks_for_one() {
    let mut connection = attached(vec![]).await;
    let lifecycle = connection.lifecycle();
    lifecycle.reconnect().await.expect("the pump has not started yet");
    let closing = connection.run().await.expect("the request ends the pump");
    assert_eq!(closing, Disconnect::local(4900, RECONNECT_REASON));
    assert_eq!(
        connection.socket.closed,
        Some((4900, RECONNECT_REASON.to_owned())),
        "the peer is told we are leaving for another host, not that we are done"
    );
}

/// Leaving on purpose is not an outage: the guarded pump reports it and never dials again.
#[tokio::test]
async fn leaving_on_request_never_dials_another_gateway() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (dialled, saw) = mpsc::channel(1);
    drop(saw);
    let mut network = Reconnects { server, next: VecDeque::new(), dialled, intros: Vec::new() };
    let (mut connection, gateway) = driven(server).await;
    let lifecycle = connection.lifecycle();
    lifecycle.disconnect().await.expect("the pump has not started yet");
    drop(gateway);
    let closing =
        connection.pump_retrying(&mut NoFrames, &mut network).await.expect("the supervisor runs");
    assert_eq!(closing, Disconnect::local(1000, ""));
    assert!(
        network.intros.is_empty(),
        "a session we ended is not one worth re-opening: {:?}",
        network.intros
    );
}

#[tokio::test]
async fn leaving_interrupts_reconnect_backoff() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (dialled, saw) = mpsc::channel(1);
    drop(saw);
    let mut network = Reconnects { server, next: VecDeque::new(), dialled, intros: Vec::new() };
    let (mut connection, gateway) = driven(server).await;
    let lifecycle = connection.lifecycle();
    gateway.send(Inbound::Closed(4015, "retry".into())).await.expect("pump input");
    let driver = tokio::spawn(async move {
        connection.pump_retrying(&mut NoFrames, &mut network).await.expect("supervisor")
    });
    tokio::task::yield_now().await;
    lifecycle.disconnect().await.expect("queue open");
    let closing = tokio::time::timeout(Duration::from_millis(100), driver)
        .await
        .expect("disconnect does not wait for backoff")
        .expect("driver task");
    assert_eq!(closing, Disconnect::local(1000, ""));
}

/// The whole point of a resume: another gateway socket, the same voice session. UDP, transport
/// key, DAVE state, RTP clock and speaking state stay in place; no protocol selection is repeated.
#[tokio::test(start_paused = true)]
async fn a_retryable_close_reopens_the_gateway_and_keeps_the_session() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (second, queue) = mpsc::channel(8);
    let (dialled, mut saw) = mpsc::channel(4);
    let mut network = Reconnects {
        server,
        next: VecDeque::from([(42u32, true, Vec::new(), queue)]),
        dialled,
        intros: Vec::new(),
    };
    let (mut connection, gateway) = driven(server).await;
    let key = session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE);
    gateway.send(text(&key)).await.expect("the pump has not started");
    tokio::task::yield_now().await;
    let frames = connection.frames();
    let speaking = connection.speaking();
    speaking.set(SPEAKING_MICROPHONE).await.expect("live");
    frames.notify().await.expect("live");
    let mut source = Scripted::new((0..6).map(|n| Some(vec![0xa1u8; 4 + n])));
    let driver = tokio::spawn(async move {
        let closing =
            connection.pump_retrying(&mut source, &mut network).await.expect("the supervisor runs");
        (closing, network.intros, connection.frame_stats(), connection.socket.sent)
    });

    pace(3).await;
    let before = read_datagrams(&voice);
    assert_eq!(
        stream_of(&before),
        [(0, 7), (960, 7), (1920, 7)],
        "three frames on the first READY's ssrc"
    );
    let last_seq = rtp_shape(before.last().expect("three frames")).0;

    // Where the session had reached, then the voice host goes away.
    gateway
        .send(text(r#"{"op":12,"seq":9,"d":{"user_id":"9","audio_ssrc":5}}"#))
        .await
        .expect("the pump is live");
    gateway
        .send(Inbound::Closed(4015, "the voice host died".to_owned()))
        .await
        .expect("the pump is live");
    saw.recv().await.expect("a second gateway was dialled");

    // A successful op-9 resume keeps the media path and key. The paused schedule restarts now
    // instead of treating the reconnect delay as dropped audio.
    tokio::task::yield_now().await;
    pace(2).await;
    let after = read_datagrams(&voice);
    assert_eq!(
        stream_of(&after),
        [(2880, 7), (3840, 7), (4800, 7)],
        "resume keeps the original UDP stream and continues its audio clock"
    );
    assert_eq!(
        after.iter().map(|packet| rtp_shape(packet).0).collect::<Vec<_>>(),
        [last_seq + 1, last_seq + 2, last_seq + 3],
        "one sequence number per datagram, across the reconnect"
    );

    second.send(Inbound::Closed(1000, "the run ended".to_owned())).await.expect("the pump is live");
    let (closing, intros, stats, sent) = driver.await.expect("the supervisor finishes");
    assert_eq!(
        intros,
        [Intro::Resume(9)],
        "the server still holds the session at the frame we last saw"
    );
    assert_eq!(
        closing,
        Disconnect::remote(1000, "the run ended"),
        "the close the app is told about is the one that ended the run"
    );
    assert_eq!(
        stats,
        FrameStats { sent: 6, skipped: 0 },
        "the reconnect delay is not counted as dropped audio"
    );
    assert_eq!(
        sent.len(),
        1,
        "a successful resume sends only op 7, not backend/select/video/speaking"
    );
}
