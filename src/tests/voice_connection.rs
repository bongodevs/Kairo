//! The server's voice lifecycle end to end: a PATCH opens a real gateway connection, the player
//! reports it, and a DELETE closes it with a normal close frame.
//!
//! The gateway is local and plaintext (the `plaintext-gateway` test feature on the voice crate), but
//! everything between the REST handler and the wire, including UDP discovery and the protocol
//! handshake, is the production client.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, accept_async};

use crate::test_support::{app_state, request, spawn_server};

const AUTH: Option<&str> = Some("letmein");
const SSRC: u32 = 0x1234_5678;
const MODE: &str = "aead_xchacha20_poly1305_rtpsize";
const KEY: [u8; 32] = [0x5a; 32];

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("JSON response")
}

async fn next_json(ws: &mut WebSocketStream<TcpStream>) -> serde_json::Value {
    loop {
        match ws.next().await.expect("gateway frame").expect("gateway socket") {
            Message::Text(text) => return serde_json::from_str(text.as_str()).expect("JSON frame"),
            Message::Ping(payload) => {
                ws.send(Message::Pong(payload)).await.expect("pong");
            }
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(frame) => panic!("closed before the test was done: {frame:?}"),
        }
    }
}

/// Answer voice UDP discovery the way Discord does: echo the caller's address in the packet.
async fn answer_discovery(socket: UdpSocket) -> std::net::SocketAddr {
    let mut packet = [0u8; 74];
    let (read, from) = socket.recv_from(&mut packet).await.expect("discovery request");
    assert_eq!(read, 74, "one discovery packet");
    assert_eq!(&packet[..4], &[0, 1, 0, 70], "request type and length");
    assert_eq!(u32::from_be_bytes(packet[4..8].try_into().expect("ssrc")), SSRC);
    packet[0..2].copy_from_slice(&[0, 2]);
    let ip = from.ip().to_string();
    packet[8..8 + ip.len()].copy_from_slice(ip.as_bytes());
    packet[72..74].copy_from_slice(&from.port().to_be_bytes());
    socket.send_to(&packet, from).await.expect("discovery reply");
    from
}

/// Run the reference voice handshake, then wait for the server to close the socket.
async fn fake_gateway(listener: TcpListener, voice_port: u16) -> (serde_json::Value, u16) {
    let (stream, _) = listener.accept().await.expect("gateway accept");
    let mut ws = accept_async(stream).await.expect("gateway handshake");
    ws.send(Message::Text(r#"{"op":8,"d":{"heartbeat_interval":45000}}"#.into()))
        .await
        .expect("hello");
    let identify = next_json(&mut ws).await;
    assert_eq!(identify["op"], 0);
    ws.send(Message::Text(
        format!(
            r#"{{"op":2,"seq":1,"d":{{"ip":"127.0.0.1","port":{voice_port},"ssrc":{SSRC},"modes":["{MODE}"]}}}}"#
        )
        .into(),
    ))
    .await
    .expect("ready");

    // Media sink, backend version, select protocol, initial speaking and the stream announce.
    for expected in [15, 16, 1, 5, 12] {
        let frame = next_json(&mut ws).await;
        assert_eq!(frame["op"], expected);
    }

    let key = serde_json::to_string(&KEY).expect("key");
    ws.send(Message::Text(
        format!(
            r#"{{"op":4,"seq":2,"d":{{"mode":"{MODE}","secret_key":{key},"audio_codec":"opus","dave_protocol_version":0}}}}"#
        )
        .into(),
    ))
    .await
    .expect("session description");

    loop {
        match ws.next().await.expect("gateway frame").expect("gateway socket") {
            Message::Close(frame) => {
                return (identify, frame.map_or(1005, |frame| u16::from(frame.code)));
            }
            Message::Ping(payload) => ws.send(Message::Pong(payload)).await.expect("pong"),
            Message::Text(_) | Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

#[tokio::test]
async fn a_player_voice_connection_is_reported_and_closed_through_the_rest_routes() {
    let udp = UdpSocket::bind("127.0.0.1:0").await.expect("voice udp");
    let voice_port = udp.local_addr().expect("voice udp address").port();
    let discovery = tokio::spawn(answer_discovery(udp));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("voice gateway");
    let gateway_port = listener.local_addr().expect("voice gateway address").port();
    let gateway = tokio::spawn(fake_gateway(listener, voice_port));

    let state = app_state(AUTH);
    state.sessions.reserve_for("session", 123);
    let address = spawn_server(state).await;

    let body = serde_json::json!({
        "voice": {
            "token": "voice-token",
            "endpoint": format!("plaintext://127.0.0.1:{gateway_port}"),
            "sessionId": "voice-session",
            "channelId": "456"
        }
    })
    .to_string();
    let patched =
        request(address, "PATCH", "/v4/sessions/session/players/42", AUTH, Some(&body)).await;
    assert_eq!(patched.status, 200, "{}", patched.body);
    let patched = json(&patched.body);
    assert_eq!(patched["state"]["connected"], true);
    assert!(
        patched["state"]["ping"].as_i64().is_some_and(|ping| ping >= 0),
        "{}",
        patched["state"]
    );

    let deleted = request(address, "DELETE", "/v4/sessions/session/players/42", AUTH, None).await;
    assert_eq!(deleted.status, 204);

    let (identify, code) = tokio::time::timeout(Duration::from_secs(5), gateway)
        .await
        .expect("gateway close timeout")
        .expect("gateway task");
    assert_eq!(code, 1000, "deleting the player sends a normal close frame");
    assert_eq!(identify["d"]["server_id"], "42");
    assert_eq!(identify["d"]["channel_id"], "456");
    assert_eq!(identify["d"]["user_id"], "123");
    assert_eq!(identify["d"]["session_id"], "voice-session");

    let from = tokio::time::timeout(Duration::from_secs(5), discovery)
        .await
        .expect("discovery timeout")
        .expect("discovery task");
    assert_eq!(from.ip().to_string(), "127.0.0.1");

    let missing = request(address, "GET", "/v4/sessions/session/players/42", AUTH, None).await;
    assert_eq!(missing.status, 404);
}
