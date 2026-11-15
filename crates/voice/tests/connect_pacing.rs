use super::*;
#[tokio::test(start_paused = true)]
async fn a_producer_starts_the_cadence_and_the_tail_stops_it() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (mut connection, inbound) = driven(server).await;
    assert!(!connection.ready_to_send(), "no key until op 4");

    let mut source = Scripted::new([Some(vec![0xa1u8; 4]), Some(vec![0xa2u8; 6])]);
    let frames = connection.frames();
    let driver = tokio::spawn(async move {
        let code = connection.run_with(&mut source).await.expect("pump runs").code;
        (code, connection.frame_stats(), source.pulled, announced(&connection.socket.sent))
    });

    // The key first: a tick before it is spent sending nothing.
    inbound.send(text(&session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE))).await.unwrap();
    tokio::task::yield_now().await;
    frames.notify().await.expect("the pump is live");

    // Eight slots: two frames, the five-frame tail, then the tick that finds nothing left.
    pace(8).await;
    let packets = read_datagrams(&voice);
    assert_eq!(packets.len(), 7, "two real frames and the whole silence tail");
    let shapes: Vec<(u16, u32, usize, u32, u32)> =
        packets.iter().map(|packet| rtp_shape(packet)).collect();
    assert!(shapes.iter().all(|shape| shape.4 == 7), "READY's ssrc keys the stream");
    assert_eq!(
        shapes.iter().map(|shape| shape.1).collect::<Vec<_>>(),
        [0, 960, 1920, 2880, 3840, 4800, 5760],
        "one frame of audio time per packet, counted only for packets written"
    );
    assert_eq!(
        shapes.iter().map(|shape| shape.2).collect::<Vec<_>>(),
        [4, 6, 3, 3, 3, 3, 3],
        "the tail is the silence packet, whatever the frames were"
    );
    let first_seq = shapes[0].0;
    assert_eq!(
        shapes.iter().map(|shape| shape.0).collect::<Vec<_>>(),
        (0..7u16).map(|offset| first_seq + offset).collect::<Vec<_>>(),
        "one sequence number per datagram, from the random start"
    );
    let first_nonce = shapes[0].3;
    assert_eq!(
        shapes.iter().map(|shape| shape.3).collect::<Vec<_>>(),
        (0..7u32).map(|offset| first_nonce + offset).collect::<Vec<_>>(),
        "the nonce prefix is the per-packet counter"
    );

    // At rest the schedule holds no timer and polls nothing.
    pace(20).await;
    assert!(read_datagrams(&voice).is_empty(), "no polling while idle");
    drop(inbound);
    let (code, stats, pulled, speaking) = driver.await.expect("the driver finishes");
    assert_eq!(code, 1000, "the gateway closed");
    assert_eq!(stats, FrameStats { sent: 7, skipped: 0 }, "every scheduled frame went out");
    assert_eq!(pulled, 8, "the source is not asked again once pacing stopped");
    assert_eq!(speaking, [0, 1], "audio starts speaking and stops silently");
}

#[tokio::test(start_paused = true)]
async fn while_the_gate_is_shut_the_pace_is_kept_and_nothing_is_sent() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (mut connection, inbound) = driven(server).await;
    let frames = connection.frames();
    let mut source = Scripted::new([Some(vec![0xa1u8; 4])]);
    let driver = tokio::spawn(async move {
        let code = connection.run_with(&mut source).await.expect("pump runs").code;
        (code, connection.frame_stats(), source.pulled)
    });
    frames.notify().await.expect("the pump is live");

    pace(5).await;
    assert!(read_datagrams(&voice).is_empty(), "a frame before the key would be leaked or chopped");

    // The open gate does not restart the clock: the next slot is 20 ms after the last tick,
    // not 20 ms after the description arrived.
    inbound.send(text(&session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE))).await.unwrap();
    tokio::task::yield_now().await;
    pace(1).await;
    let packets = read_datagrams(&voice);
    assert_eq!(packets.len(), 1, "one slot, one frame");
    assert_eq!(rtp_shape(&packets[0]).1, 0, "the first audio timestamp");
    drop(inbound);
    let (code, stats, pulled) = driver.await.expect("the driver finishes");
    assert_eq!(code, 1000);
    assert_eq!(
        stats,
        FrameStats { sent: 1, skipped: 0 },
        "a shut gate skips the send, never counts a lost frame"
    );
    assert_eq!(pulled, 1, "the source is not even polled while the gate is shut");
}

#[tokio::test(start_paused = true)]
async fn an_oversized_frame_is_refused_and_the_next_one_still_goes_out() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (mut connection, inbound) = driven(server).await;
    inbound.send(text(&session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE))).await.unwrap();
    tokio::task::yield_now().await;
    let frames = connection.frames();
    // First a frame longer than the buffer the sender hands out, then a usable one.
    let mut source =
        Scripted::new([Some(vec![0xa1u8; MAX_PACKET_BYTES + 1]), Some(vec![0xa2u8; 4])]);
    let driver = tokio::spawn(async move {
        let code = connection.run_with(&mut source).await.expect("pump runs").code;
        (code, connection.frame_stats())
    });
    frames.notify().await.expect("the pump is live");

    pace(2).await;
    let packets = read_datagrams(&voice);
    assert_eq!(packets.len(), 1, "a refused frame never reaches the wire");
    let (_seq, timestamp, encrypted, _nonce, _ssrc) = rtp_shape(&packets[0]);
    assert_eq!(timestamp, 0, "a skipped frame never advances the RTP counters");
    assert_eq!(encrypted, 4, "the next scheduled frame went out whole");
    drop(inbound);
    let (code, stats) = driver.await.expect("the driver keeps running");
    assert_eq!(code, 1000, "a bad frame never ends the connection");
    assert_eq!(
        stats,
        FrameStats { sent: 1, skipped: 1 },
        "the refused frame is counted as skipped, the written one as sent"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unwritable_socket_counts_skips_and_keeps_reading_the_gateway() {
    let (sender, queue) = mpsc::channel(8);
    let gateway = Gateway::establish(
        LiveSocket::new(hello_frames(), queue),
        &target(),
        Intro::Identify,
        VoiceConfig::default(),
    )
    .await
    .expect("handshake");
    // Bound but never connected: every write fails, which is a skipped frame, not a dead pump.
    let udp = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let local = udp.local_addr().unwrap();
    let mut connection =
        Connection::attach(gateway, &target(), media_path(udp, local, 7), VoiceConfig::default())
            .await
            .expect("select-protocol");
    let key = session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE);
    let mut source = Scripted::new([Some(vec![0xa1u8; 4]), None]);
    let frames = connection.frames();
    let speaking = connection.speaking();
    sender.send(text(&key)).await.expect("the pump has not started yet");
    frames.notify().await.expect("the pump is live");
    let driver = tokio::spawn(async move {
        let code = connection.run_with(&mut source).await.expect("pump runs").code;
        (code, connection.frame_stats())
    });

    pace(3).await;
    speaking.set(SPEAKING_MICROPHONE).await.expect("the pump still reads its commands");
    tokio::task::yield_now().await;
    drop(sender);
    let (code, stats) = driver.await.expect("the driver finishes");
    assert_eq!(code, 1000);
    assert_eq!(
        stats,
        FrameStats { sent: 0, skipped: 3 },
        "frames the socket refused are skipped, and the silence tail is owed all the same"
    );
}
