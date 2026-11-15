use std::time::Duration;

use super::*;
use crate::connection::disconnect::Disconnect;
use crate::gateway::heartbeat::Heartbeat;
use crate::gateway::pump::{Pump, heartbeat_loop};
use crate::gateway::wire::{VoiceEvent, split_dave_frame};
use crate::gateway::{Handled, json_reply};
#[tokio::test(start_paused = true)]
async fn heartbeats_on_schedule_and_ack_updates_seq_and_ping() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let mut hb = Heartbeat::new(Duration::from_millis(100), None);

    let driver = async {
        tokio::time::sleep(Duration::from_millis(350)).await;
        tx.send(Inbound::Text(r#"{"op":6,"seq":7}"#.to_string())).unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        drop(tx);
    };

    let mut controls = no_controls();
    let (res, ()) = tokio::join!(
        heartbeat_loop(&mut socket, &mut hb, &mut controls, |_| {
            Ok::<_, GatewayError>(Handled::replies(Vec::new()))
        }),
        driver
    );
    let closing = res.unwrap();
    assert_eq!(closing.code, 1006, "a stream that ends with nothing said is an abnormal closure");
    assert!(!closing.by_remote, "nobody told us anything");

    assert_eq!(socket.sent.len(), 4, "beats at 100/200/300/400 ms");
    let first: serde_json::Value = serde_json::from_str(&socket.sent[0]).unwrap();
    assert_eq!(first["op"], 3);
    assert!(first["d"]["t"].as_u64().is_some());
    assert_eq!(first["d"]["seq_ack"], -1, "nothing acknowledged yet is -1, not null");
    let last: serde_json::Value = serde_json::from_str(socket.sent.last().unwrap()).unwrap();
    assert_eq!(last["d"]["seq_ack"], 7, "beat after the ack carries the new seq");
    assert_eq!(hb.seq_ack, Some(7));
    assert!(hb.ping < Duration::from_secs(5), "ping {} ms", hb.ping.as_millis());
}

#[tokio::test(start_paused = true)]
async fn other_frames_are_routed_and_can_stop_the_pump() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let mut hb = Heartbeat::new(Duration::from_secs(3600), None);
    tx.send(Inbound::Text(r#"{"op":5,"seq":9,"d":{"user_id":"42"}}"#.to_string())).unwrap();
    drop(tx);

    let mut seen = Vec::new();
    let mut controls = no_controls();
    heartbeat_loop(&mut socket, &mut hb, &mut controls, |pump| {
        seen.push(match pump {
            Pump::Frame(event) => event,
            Pump::Control(()) | Pump::Tick => unreachable!("no controls, no deadline"),
        });
        Err(GatewayError::Protocol("stop".into()))
    })
    .await
    .expect_err("closure error stops the pump");
    match &seen[0] {
        VoiceEvent::Json { op, d } => {
            assert_eq!(*op, 5);
            assert_eq!(d.as_ref().unwrap()["user_id"], "42");
        }
        other => panic!("expected a JSON frame, got {other:?}"),
    }
    assert_eq!(hb.seq_ack, Some(9), "seq updates even for routed frames");
}

#[tokio::test(start_paused = true)]
async fn binary_dave_frames_are_split_and_ack_their_seq() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let mut hb = Heartbeat::new(Duration::from_secs(3600), None);
    // [seq=7 u16 BE][op=25][body 0xAA 0xBB]
    tx.send(Inbound::Binary(vec![0x00, 0x07, 25, 0xAA, 0xBB])).unwrap();
    drop(tx);

    let mut seen = Vec::new();
    let mut controls = no_controls();
    heartbeat_loop(&mut socket, &mut hb, &mut controls, |pump| {
        seen.push(match pump {
            Pump::Frame(event) => event,
            Pump::Control(()) | Pump::Tick => unreachable!("no controls, no deadline"),
        });
        Ok::<_, GatewayError>(Handled::replies(Vec::new()))
    })
    .await
    .expect("pump runs");
    match &seen[0] {
        VoiceEvent::Dave { op, body } => {
            assert_eq!(*op, 25);
            assert_eq!(body, &[0xAA, 0xBB], "header stripped, body kept");
        }
        other => panic!("expected a DAVE frame, got {other:?}"),
    }
    assert_eq!(hb.seq_ack, Some(7), "binary seq becomes the heartbeat ack");
}

#[tokio::test(start_paused = true)]
async fn controls_are_written_without_a_frame_or_a_beat() {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let mut hb = Heartbeat::new(Duration::from_secs(3600), None);
    let (ctx, mut controls) = mpsc::channel(4);
    // Queued before the pump runs, and the only inbound is the close: nothing but the
    // control branch can put these on the wire.
    ctx.send(1u8).await.expect("the queue is open");
    ctx.send(4u8).await.expect("the queue is open");
    tx.send(Inbound::Closed(1000, "bye".into())).unwrap();

    let mut masks = Vec::new();
    let closing = heartbeat_loop(&mut socket, &mut hb, &mut controls, |pump| {
        let mask = match pump {
            Pump::Control(mask) => mask,
            Pump::Frame(_) | Pump::Tick => {
                return Err(GatewayError::Protocol("only a close is inbound".into()));
            }
        };
        masks.push(mask);
        json_reply(5, &serde_json::json!({ "speaking": mask }))
            .map(|reply| Handled::replies(vec![reply]))
    })
    .await
    .expect("pump runs");

    assert_eq!(closing, Disconnect::remote(1000, "bye"));
    assert_eq!(masks, [1, 4], "in queue order");
    assert_eq!(socket.sent, [r#"{"op":5,"d":{"speaking":1}}"#, r#"{"op":5,"d":{"speaking":4}}"#]);
    assert_eq!(hb.ping, Duration::ZERO, "no beat was due");
}

#[test]
fn short_dave_frames_are_rejected() {
    for len in 0..3 {
        let frame = vec![0u8; len];
        assert!(
            matches!(split_dave_frame(&frame), Err(GatewayError::Protocol(_))),
            "{len} bytes must be a protocol error"
        );
    }
    let (seq, op, body) = split_dave_frame(&[0x12, 0x34, 29, 0x01]).expect("valid");
    assert_eq!(seq, 0x1234);
    assert_eq!(op, 29);
    assert_eq!(body, &[0x01]);
}

#[tokio::test(start_paused = true)]
async fn pump_reports_a_close_the_peer_sent() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let mut hb = Heartbeat::new(Duration::from_secs(3600), None);
    tx.send(Inbound::Closed(4015, "voice server crashed".into())).unwrap();
    let mut controls = no_controls();
    let closing = heartbeat_loop(&mut socket, &mut hb, &mut controls, |_| {
        Ok::<_, GatewayError>(Handled::replies(Vec::new()))
    })
    .await
    .unwrap();
    assert_eq!(closing, Disconnect::remote(4015, "voice server crashed"));
    assert_eq!(socket.closed, None, "a close we did not ask for is not answered with another one");
}

/// What V12 adds to the pump: the handler can end the session itself, and the report then says
/// we did it.
#[tokio::test(start_paused = true)]
async fn a_handler_that_ends_the_session_closes_the_socket_first() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let mut hb = Heartbeat::new(Duration::from_secs(3600), None);
    let (ctx, mut controls) = mpsc::channel(4);
    ctx.send(1u8).await.expect("the queue is open");
    drop(tx);

    let closing = heartbeat_loop(&mut socket, &mut hb, &mut controls, |pump| {
        let mask = match pump {
            Pump::Control(mask) => mask,
            Pump::Frame(_) | Pump::Tick => {
                return Err(GatewayError::Protocol("only a control is queued".into()));
            }
        };
        let reply = json_reply(5, &serde_json::json!({ "speaking": mask }))?;
        Ok::<_, GatewayError>(Handled {
            replies: vec![reply],
            wake: None,
            end: Some(Disconnect::local(1000, "")),
        })
    })
    .await
    .expect("pump ends on the requested close");

    assert_eq!(closing, Disconnect::local(1000, ""));
    assert_eq!(
        socket.sent,
        [r#"{"op":5,"d":{"speaking":1}}"#],
        "what the handler queued goes out before the close"
    );
    assert_eq!(
        socket.closed,
        Some((1000, String::new())),
        "and the peer is told why before the socket goes"
    );
}

#[tokio::test(start_paused = true)]
async fn ending_the_session_never_puts_1006_on_the_wire() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = ChannelSocket { rx, sent: Vec::new(), closed: None };
    let mut hb = Heartbeat::new(Duration::from_secs(3600), None);
    let (ctx, mut controls) = mpsc::channel(4);
    ctx.send(()).await.expect("the queue is open");
    drop(tx);

    let closing = heartbeat_loop(&mut socket, &mut hb, &mut controls, |pump| {
        match pump {
            Pump::Control(()) => {}
            Pump::Frame(_) | Pump::Tick => {
                return Err(GatewayError::Protocol("not reached".into()));
            }
        }
        Ok::<_, GatewayError>(Handled {
            // The code a stream that vanished would report, asked for on purpose this time.
            replies: Vec::new(),
            wake: None,
            end: Some(Disconnect::abnormal("gone")),
        })
    })
    .await
    .expect("pump ends");

    assert_eq!(closing, Disconnect::abnormal("gone"));
    assert!(socket.closed.is_none(), "1006 is what a peer infers, never something we send");
}
