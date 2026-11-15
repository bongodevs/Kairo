//! The same driver over real sockets: a local websocket voice gateway and a local UDP server that
//! answers discovery, so everything except the wss handshake itself is production code.

use super::*;
/// The driver end to end over real sockets: a local websocket voice gateway and a local UDP
/// server that answers discovery. Everything except the wss handshake itself is the code
/// production runs, so the frames on the wire are the ones Discord will see.
#[tokio::test]
pub(super) async fn a_connection_runs_against_a_local_voice_server() {
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::{MaybeTlsStream, accept_async, client_async};

    const SSRC: u32 = 0x1234_5678;
    let key = [0x5au8; KEY_LEN];

    // UDP: reply to the discovery request with the source address we were called from.
    let voice_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let voice_udp_port = voice_udp.local_addr().unwrap().port();
    let discovered = tokio::spawn(async move {
        let mut packet = [0u8; 74];
        let (read, from) = voice_udp.recv_from(&mut packet).await.unwrap();
        assert_eq!(read, 74, "one discovery packet");
        assert_eq!(&packet[..4], &[0, 1, 0, 70], "request type and length");
        assert_eq!(u32::from_be_bytes(packet[4..8].try_into().unwrap()), SSRC, "ssrc echoes READY");
        packet[0..2].copy_from_slice(&[0, 2]);
        let ip = from.ip().to_string();
        packet[8..8 + ip.len()].copy_from_slice(ip.as_bytes());
        packet[72..74].copy_from_slice(&from.port().to_be_bytes());
        voice_udp.send_to(&packet, from).await.unwrap();
        from
    });

    // Gateway: hello, identify, ready, sink-wants, backend-version, select, speaking-zero, video, description, close.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_port = listener.local_addr().unwrap().port();
    let gateway_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        ws.send(Message::Text(r#"{"op":8,"d":{"heartbeat_interval":45000}}"#.into()))
            .await
            .unwrap();
        let Message::Text(identify) = ws.next().await.unwrap().unwrap() else {
            panic!("expected a text frame");
        };
        ws.send(Message::Text(
            format!(
                r#"{{"op":2,"seq":1,"d":{{"ip":"127.0.0.1","port":{voice_udp_port},"ssrc":{SSRC},"modes":["aead_xchacha20_poly1305_rtpsize"]}}}}"#
            )
            .into(),
        ))
        .await
        .unwrap();
        let Message::Text(media_sink) = ws.next().await.unwrap().unwrap() else {
            panic!("expected a text frame");
        };
        let Message::Text(backend_version) = ws.next().await.unwrap().unwrap() else {
            panic!("expected a text frame");
        };
        let Message::Text(select) = ws.next().await.unwrap().unwrap() else {
            panic!("expected a text frame");
        };
        let Message::Text(initial_speaking) = ws.next().await.unwrap().unwrap() else {
            panic!("expected a text frame");
        };
        let Message::Text(video) = ws.next().await.unwrap().unwrap() else {
            panic!("expected a text frame");
        };
        let key_json = serde_json::to_string(&key).unwrap();
        ws.send(Message::Text(
            format!(
                r#"{{"op":4,"seq":2,"d":{{"mode":"aead_xchacha20_poly1305_rtpsize","secret_key":{key_json},"audio_codec":"opus","dave_protocol_version":0}}}}"#
            )
            .into(),
        ))
        .await
        .unwrap();
        // Nothing more is sent from here: the next frame can only be the speaking change.
        let Message::Text(speaking) = ws.next().await.unwrap().unwrap() else {
            panic!("expected a text frame");
        };
        let close = tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: CloseCode::Normal,
            reason: "done".into(),
        };
        ws.send(Message::Close(Some(close))).await.unwrap();
        (
            identify.to_string(),
            media_sink.to_string(),
            backend_version.to_string(),
            select.to_string(),
            initial_speaking.to_string(),
            video.to_string(),
            speaking.to_string(),
        )
    });

    let stream = TcpStream::connect(("127.0.0.1", gateway_port)).await.unwrap();
    // Plain instead of TLS: the wss handshake is the one step a local server cannot stand in for.
    let (client, _) =
        client_async(format!("ws://127.0.0.1:{gateway_port}/?v=8"), MaybeTlsStream::Plain(stream))
            .await
            .unwrap();
    let target = VoiceTarget {
        endpoint: "127.0.0.1".into(),
        guild: GuildId(1),
        channel: ChannelId(2),
        user: UserId(3),
        session_id: "session".into(),
        token: "token".into(),
    };
    let mut gateway = Gateway::establish(client, &target, Intro::Identify, VoiceConfig::default())
        .await
        .expect("handshake");
    let ready = gateway.ready().cloned().expect("READY");
    assert_eq!(ready.ssrc, SSRC);
    let path =
        gateway.drive_while(transport(&ready, &VoiceConfig::default())).await.expect("discovery");
    assert_eq!(path.mode, XCHACHA_MODE);

    let mut connection = Connection::attach(gateway, &target, path, VoiceConfig::default())
        .await
        .expect("select-protocol");
    assert!(!connection.ready_to_send(), "no key until op 4");
    let speaking = connection.speaking();
    let driver = tokio::spawn(async move {
        let closing = connection.run().await.expect("pump runs");
        (
            closing,
            connection.phase(),
            connection.ready_to_send(),
            connection.mode().to_owned(),
            connection.local(),
        )
    });
    // The pump is already parked on the socket and the next heartbeat is 45 s away, so
    // only the control branch can deliver this frame.
    speaking.set(SPEAKING_MICROPHONE).await.expect("the pump is live");
    let (closing, phase, ready_to_send, negotiated, advertised) =
        driver.await.expect("the driver finishes");

    let (identify, media_sink, backend_version, select, initial_speaking, video, announced) =
        gateway_task.await.unwrap();
    let from_udp = discovered.await.unwrap();
    assert_eq!(
        closing,
        Disconnect::remote(1000, "done"),
        "a close frame from the peer is reported as the peer's"
    );
    assert_eq!(phase, Phase::Ready);
    assert!(ready_to_send, "the session key is in place");
    assert_eq!(negotiated, XCHACHA_MODE);
    assert_eq!(advertised, from_udp, "we advertise what we found");

    let identify: Value = serde_json::from_str(&identify).unwrap();
    assert_eq!(identify["op"], 0);
    assert_eq!(identify["d"]["server_id"], "1");
    assert_eq!(identify["d"]["channel_id"], "2");
    assert_eq!(identify["d"]["max_dave_protocol_version"], 1);
    let media_sink: Value = serde_json::from_str(&media_sink).unwrap();
    assert_eq!(media_sink["op"], 15);
    assert_eq!(media_sink["d"]["any"], 100);
    let backend_version: Value = serde_json::from_str(&backend_version).unwrap();
    assert_eq!(backend_version["op"], 16, "READY is answered by a version ask");
    assert_eq!(backend_version["d"], Value::Object(Default::default()));
    let select: Value = serde_json::from_str(&select).unwrap();
    assert_eq!(select["op"], 1);
    assert_eq!(select["d"]["protocol"], "udp");
    assert_eq!(
        select["d"]["data"]["address"],
        from_udp.ip().to_string(),
        "the discovered address is announced"
    );
    assert_eq!(select["d"]["data"]["port"], from_udp.port());
    assert_eq!(select["d"]["mode"], negotiated, "top-level copy agrees with data");
    assert_eq!(select["d"]["codecs"][0]["payload_type"], 120);

    let initial_speaking: Value = serde_json::from_str(&initial_speaking).unwrap();
    assert_eq!(initial_speaking["op"], 5);
    assert_eq!(initial_speaking["d"]["speaking"], 0);
    assert_eq!(initial_speaking["d"]["ssrc"], u64::from(SSRC));

    let video: Value = serde_json::from_str(&video).unwrap();
    assert_eq!(video["op"], 12, "select-protocol is followed by the stream announce");
    assert_eq!(video["d"]["audio_ssrc"], u64::from(SSRC));
    assert_eq!(video["d"]["video_ssrc"], 0);
    assert_eq!(video["d"]["rtx_ssrc"], 0);
    let announced: Value = serde_json::from_str(&announced).unwrap();
    assert_eq!(announced["op"], 5, "the mask change reached the wire");
    assert_eq!(announced["d"]["speaking"], 1);
    assert_eq!(announced["d"]["delay"], 0);
    assert_eq!(announced["d"]["ssrc"], u64::from(SSRC));
}

/// A driver whose gateway stays open, plus the handle to feed it frames.
pub(super) async fn driven(
    voice_server: SocketAddr,
) -> (Connection<LiveSocket>, mpsc::Sender<Inbound>) {
    let (sender, queue) = mpsc::channel(8);
    let gateway = Gateway::establish(
        LiveSocket::new(hello_frames(), queue),
        &target(),
        Intro::Identify,
        VoiceConfig::default(),
    )
    .await
    .expect("handshake");
    let udp = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    udp.connect(voice_server).await.unwrap();
    let local = udp.local_addr().unwrap();
    let connection =
        Connection::attach(gateway, &target(), media_path(udp, local, 7), VoiceConfig::default())
            .await
            .expect("select-protocol");
    (connection, sender)
}

/// Everything waiting on the voice server's socket right now; it never blocks.
pub(super) fn read_datagrams(voice: &UdpSocket) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    loop {
        let mut buffer = vec![0u8; MAX_PACKET_BYTES];
        match voice.try_recv_from(&mut buffer) {
            Ok((len, _from)) => packets.push(buffer[..len].to_vec()),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return packets,
            Err(err) => panic!("the voice socket is unreadable: {err}"),
        }
    }
}

/// `[header 12][ciphertext][tag 16][nonce prefix 4]`: the sequence number, the timestamp, the
/// encrypted length (which is the frame length), the nonce and the ssrc the stream is keyed on.
pub(super) fn rtp_shape(packet: &[u8]) -> (u16, u32, usize, u32, u32) {
    assert!(packet.len() > RTP_HEADER_LEN + 16 + 4, "a datagram shorter than the framing");
    assert_eq!(packet[0], 0x80, "v2, no extension, no padding");
    assert_eq!(packet[1] & 0x7f, OPUS_PAYLOAD_TYPE);
    let seq = u16::from_be_bytes(packet[2..4].try_into().unwrap());
    let timestamp = u32::from_be_bytes(packet[4..8].try_into().unwrap());
    let ssrc = u32::from_be_bytes(packet[8..12].try_into().unwrap());
    let nonce = u32::from_le_bytes(packet[packet.len() - 4..].try_into().unwrap());
    (seq, timestamp, packet.len() - (RTP_HEADER_LEN + 16 + 4), nonce, ssrc)
}

/// Walk the clock in whole frames and let the parked pump run each one.
pub(super) async fn pace(frames: u32) {
    for _ in 0..frames {
        tokio::time::advance(FRAME_INTERVAL).await;
        tokio::task::yield_now().await;
    }
}
