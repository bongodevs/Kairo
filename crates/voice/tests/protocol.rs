//! The wire protocol against a scripted socket: nothing here touches a network or a real clock.

#![allow(dead_code)]

use std::collections::VecDeque;

use tokio::sync::mpsc;

use super::Heartbeat;
use crate::gateway::{GatewayError, Inbound, Socket, VoiceTarget};
use crate::ids::{ChannelId, GuildId, UserId};
#[path = "protocol_handshake.rs"]
mod handshake;
#[path = "protocol_pump.rs"]
mod pump;

#[test]
fn ping_handle_survives_heartbeat_replacement() {
    let mut heartbeat = Heartbeat::new(std::time::Duration::from_secs(1), None);
    let ping = heartbeat.ping_handle();
    heartbeat.sent_at_ms = Some(0);
    heartbeat.acknowledged();
    let first = ping.millis();
    assert!(first > 0);

    let mut replacement = Heartbeat::new(std::time::Duration::from_secs(1), None);
    replacement.inherit_ping(&heartbeat);
    replacement.sent_at_ms = Some(0);
    replacement.acknowledged();
    assert_eq!(ping.millis(), replacement.ping_handle().millis());
}

/// A control queue that is already closed: these tests exercise the socket side only.
fn no_controls() -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel(1);
    drop(tx);
    rx
}

struct MockSocket {
    incoming: VecDeque<Option<String>>,
    sent: Vec<String>,
}

impl MockSocket {
    fn new(frames: &[&str]) -> Self {
        Self {
            incoming: frames.iter().map(|frame| Some((*frame).to_string())).chain([None]).collect(),
            sent: Vec::new(),
        }
    }
}

impl Socket for MockSocket {
    async fn send_text(&mut self, text: String) -> Result<(), GatewayError> {
        self.sent.push(text);
        Ok(())
    }

    async fn send_binary(&mut self, _bytes: Vec<u8>) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Inbound>, GatewayError> {
        Ok(self.incoming.pop_front().flatten().map(Inbound::Text))
    }

    async fn close(&mut self, code: u16, reason: String) {
        self.sent.push(format!("close {code} {reason}"));
    }
}

struct ChannelSocket {
    rx: mpsc::UnboundedReceiver<Inbound>,
    sent: Vec<String>,
    closed: Option<(u16, String)>,
}

impl Socket for ChannelSocket {
    async fn send_text(&mut self, text: String) -> Result<(), GatewayError> {
        self.sent.push(text);
        Ok(())
    }

    async fn send_binary(&mut self, _bytes: Vec<u8>) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Inbound>, GatewayError> {
        Ok(self.rx.recv().await)
    }

    async fn close(&mut self, code: u16, reason: String) {
        self.closed = Some((code, reason));
    }
}

fn target() -> VoiceTarget {
    VoiceTarget {
        endpoint: "voice.example.com".into(),
        guild: GuildId(111),
        channel: ChannelId(222),
        user: UserId(333),
        session_id: "discord-session".into(),
        token: "voice-token".into(),
    }
}

const HELLO: &str = r#"{"op":8,"d":{"heartbeat_interval":13750.0}}"#;
const READY: &str = concat!(
    r#"{"op":2,"seq":1,"d":{"ip":"1.2.3.4","port":50001,"ssrc":4294967295,"#,
    r#""modes":["aead_xchacha20_poly1305_rtpsize"]}}"#
);
