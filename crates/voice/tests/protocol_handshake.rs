use std::time::Duration;

use super::*;
use crate::gateway::handshake::{HANDSHAKE_TIMEOUT, Intro, Opening, handshake};
#[tokio::test]
async fn identify_sends_v8_shape_then_waits_for_ready() {
    let mut socket = MockSocket::new(&[HELLO, READY]);
    let handshake = handshake(&mut socket, &target(), Intro::Identify).await.expect("handshake");

    let sent: serde_json::Value = serde_json::from_str(&socket.sent[0]).expect("identify is json");
    assert_eq!(sent["op"], 0);
    assert_eq!(sent["d"]["server_id"], "111");
    assert_eq!(sent["d"]["user_id"], "333");
    assert_eq!(sent["d"]["session_id"], "discord-session");
    assert_eq!(sent["d"]["token"], "voice-token");
    assert_eq!(sent["d"]["video"], true);
    assert_eq!(sent["d"]["channel_id"], "222");
    assert_eq!(sent["d"]["max_dave_protocol_version"], 1);

    let Opening::Ready(ready) = handshake.opening else {
        panic!("identify was not READY");
    };
    assert_eq!(ready.ssrc, u32::MAX);
    assert_eq!(handshake.heartbeat.interval, Duration::from_millis(13750));
    assert_eq!(handshake.heartbeat.seq_ack, Some(1));
}

#[tokio::test]
async fn resume_sends_op7_with_seq_ack_then_waits_for_resumed() {
    let mut socket = MockSocket::new(&[
        HELLO,
        r#"{"op":11,"seq":4,"d":{"user_ids":["9"]}}"#,
        r#"{"op":9,"seq":5,"d":null}"#,
    ]);
    let handshake =
        handshake(&mut socket, &target(), Intro::Resume(3)).await.expect("resume handshake");
    assert!(matches!(handshake.opening, Opening::Resumed));
    assert_eq!(handshake.heartbeat.seq_ack, Some(5));
    assert_eq!(handshake.pending.len(), 2, "op 11 and op 9 are replayed");
    let sent: serde_json::Value = serde_json::from_str(&socket.sent[0]).unwrap();
    assert_eq!(sent["op"], 7);
    assert_eq!(sent["d"]["server_id"], "111");
    assert_eq!(sent["d"]["session_id"], "discord-session");
    assert_eq!(sent["d"]["token"], "voice-token");
    assert_eq!(sent["d"]["video"], true);
    assert_eq!(sent["d"]["seq_ack"], 3);
    assert!(sent["d"].get("channel_id").is_none(), "resume carries no channel");
}

#[tokio::test(start_paused = true)]
async fn heartbeat_starts_at_hello_before_ready() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let server = async move {
        tx.send(Inbound::Text(r#"{"op":8,"d":{"heartbeat_interval":1000}}"#.into()))
            .expect("hello");
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        tx.send(Inbound::Text(READY.into())).expect("ready");
    };
    let target = target();
    let (handshake, ()) = tokio::join!(handshake(&mut socket, &target, Intro::Identify), server);
    handshake.expect("handshake");
    let ops = socket
        .sent
        .iter()
        .map(|frame| serde_json::from_str::<serde_json::Value>(frame).unwrap()["op"].clone())
        .collect::<Vec<_>>();
    assert_eq!(ops, [0, 3], "identify, then a heartbeat before READY");
}

#[tokio::test]
async fn a_resume_that_saw_nothing_acks_minus_one() {
    // The server reads -1 as "nothing acknowledged yet"; a 0 would claim one delivered message
    // and suppress the redelivery of whatever arrived first.
    let mut socket = MockSocket::new(&[HELLO, r#"{"op":9,"d":null}"#]);
    handshake(&mut socket, &target(), Intro::Resume(-1)).await.expect("resume handshake");
    let sent: serde_json::Value = serde_json::from_str(&socket.sent[0]).unwrap();
    assert_eq!(sent["d"]["seq_ack"], -1);
}

#[tokio::test]
async fn events_before_ready_are_buffered() {
    let mut socket = MockSocket::new(&[HELLO, r#"{"op":13,"d":{}}"#, READY]);
    socket.incoming.insert(1, Some(r#"{"op":5,"d":{"user_id":"9"}}"#.into()));
    let handshake = handshake(&mut socket, &target(), Intro::Identify)
        .await
        .expect("events must not break handshake");
    assert_eq!(handshake.pending.len(), 2);
}

#[tokio::test]
async fn garbage_frame_is_a_protocol_error() {
    let mut socket = MockSocket::new(&[HELLO, "not json"]);
    let err =
        handshake(&mut socket, &target(), Intro::Identify).await.expect_err("garbage must fail");
    assert!(matches!(err, GatewayError::Protocol(_)), "{err}");
}

#[tokio::test]
async fn closed_socket_before_ready_is_reported() {
    let mut socket = MockSocket::new(&[]);
    let err = handshake(&mut socket, &target(), Intro::Identify).await.expect_err("no frames");
    assert!(matches!(err, GatewayError::Closed), "{err}");
}

#[tokio::test(start_paused = true)]
async fn handshake_times_out_without_frames() {
    struct Stalled;
    impl Socket for Stalled {
        async fn send_text(&mut self, _text: String) -> Result<(), GatewayError> {
            Ok(())
        }
        async fn send_binary(&mut self, _bytes: Vec<u8>) -> Result<(), GatewayError> {
            Ok(())
        }
        async fn recv(&mut self) -> Result<Option<Inbound>, GatewayError> {
            tokio::time::sleep(HANDSHAKE_TIMEOUT * 10).await;
            Ok(None)
        }
        async fn close(&mut self, _code: u16, _reason: String) {}
    }
    let err = handshake(&mut Stalled, &target(), Intro::Identify).await.expect_err("must time out");
    assert!(matches!(err, GatewayError::Timeout), "{err}");
}
