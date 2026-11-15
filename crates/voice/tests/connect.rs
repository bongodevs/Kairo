//! The driver, against sockets that are scripted: what the app asks for, what the server says, and
//! what has to reach the wire in between. A real local voice server and the ignored measurements
//! drive the same code through the same handles.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::path::{Path, negotiated_mode, transport, voice_server};
use super::reopen::{Redial, Reopened};
use super::wire::{OPUS_PRIORITY, connection_id};
use super::*;
use crate::config::VoiceConfig;
use crate::connection::disconnect::{Disconnect, RECONNECT_REASON};
use crate::crypto::{KEY_LEN, MAX_DATAGRAM_BYTES, MAX_PACKET_BYTES, XCHACHA_MODE};
use crate::gateway::{Gateway, GatewayError, Inbound, Intro, Ready, Socket, VoiceTarget};
use crate::ids::{ChannelId, GuildId, UserId};
use crate::scale;
use crate::sender::pacing::FRAME_INTERVAL;
use crate::sender::{COMMAND_QUEUE, FrameSource, FrameStats, Media, NoFrames};
use crate::udp::rtp::{OPUS_PAYLOAD_TYPE, RTP_HEADER_LEN};
use harness::*;
use scripts::*;
struct MockSocket {
    incoming: VecDeque<Inbound>,
    sent: Vec<String>,
    sent_binary: Vec<Vec<u8>>,
    closed: Option<(u16, String)>,
}

impl MockSocket {
    fn new(frames: Vec<Inbound>) -> Self {
        Self { incoming: frames.into(), sent: Vec::new(), sent_binary: Vec::new(), closed: None }
    }

    fn json(&self, index: usize) -> Value {
        serde_json::from_str(&self.sent[index]).expect("sent frame is JSON")
    }

    fn sent_json_of(&self, op: u64) -> Option<Value> {
        self.sent
            .iter()
            .map(|text| serde_json::from_str::<Value>(text).expect("sent frame is JSON"))
            .find(|frame| frame["op"] == op)
    }
}

impl Socket for MockSocket {
    async fn send_text(&mut self, text: String) -> Result<(), GatewayError> {
        self.sent.push(text);
        Ok(())
    }

    async fn send_binary(&mut self, bytes: Vec<u8>) -> Result<(), GatewayError> {
        self.sent_binary.push(bytes);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Inbound>, GatewayError> {
        Ok(self.incoming.pop_front())
    }

    async fn close(&mut self, code: u16, reason: String) {
        self.closed = Some((code, reason));
    }
}

fn text(body: &str) -> Inbound {
    Inbound::Text(body.to_owned())
}

/// The mock gateway with no end of input: `recv` waits on a queue the test pushes into, so a
/// test can watch the pump idle between frames and close it by dropping the sender.
struct LiveSocket {
    incoming: VecDeque<Inbound>,
    queue: mpsc::Receiver<Inbound>,
    sent: Vec<String>,
    closed: Option<(u16, String)>,
}

impl LiveSocket {
    fn new(frames: Vec<Inbound>, queue: mpsc::Receiver<Inbound>) -> Self {
        Self { incoming: frames.into(), queue, sent: Vec::new(), closed: None }
    }
}

impl Socket for LiveSocket {
    async fn send_text(&mut self, text: String) -> Result<(), GatewayError> {
        self.sent.push(text);
        Ok(())
    }

    async fn send_binary(&mut self, _bytes: Vec<u8>) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Inbound>, GatewayError> {
        if let Some(frame) = self.incoming.pop_front() {
            return Ok(Some(frame));
        }
        Ok(match self.queue.recv().await {
            Some(frame) => Some(frame),
            None => Some(Inbound::Closed(1000, "the test hung up".to_owned())),
        })
    }

    async fn close(&mut self, code: u16, reason: String) {
        self.closed = Some((code, reason));
    }
}

/// A frame source that hands out a fixed script, then stays dry forever.
struct Scripted {
    script: VecDeque<Option<Vec<u8>>>,
    /// How many times the sender asked, so a test can see the polling stop.
    pulled: usize,
    active: bool,
}

impl Scripted {
    fn new(script: impl IntoIterator<Item = Option<Vec<u8>>>) -> Self {
        Self { script: script.into_iter().collect(), pulled: 0, active: false }
    }

    fn active(mut self) -> Self {
        self.active = true;
        self
    }
}

impl FrameSource for Scripted {
    fn provide(&mut self, out: &mut [u8]) -> Option<usize> {
        self.pulled += 1;
        // A `None` entry means the source is dry; an exhausted script stays dry.
        let Some(Some(packet)) = self.script.pop_front() else {
            return None;
        };
        if packet.len() <= out.len() {
            out[..packet.len()].copy_from_slice(&packet);
        }
        // Reporting more than the buffer holds is the bug case, not the copy case.
        Some(packet.len())
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

/// READY offering the one mode this build supports, so the choice is deterministic.
fn hello_frames() -> Vec<Inbound> {
    gateway_opening(7)
}

/// HELLO plus a READY naming `ssrc`: everything a mock gateway needs before a handshake ends.
fn gateway_opening(ssrc: u32) -> Vec<Inbound> {
    vec![
        text(r#"{"op":8,"d":{"heartbeat_interval":45000}}"#),
        text(&format!(
            r#"{{"op":2,"seq":1,"d":{{"ip":"127.0.0.1","port":50002,"ssrc":{ssrc},"modes":["aead_xchacha20_poly1305_rtpsize"]}}}}"#
        )),
    ]
}

fn target() -> VoiceTarget {
    VoiceTarget {
        endpoint: "voice.example.com".into(),
        guild: GuildId(1),
        channel: ChannelId(2),
        user: UserId(3),
        session_id: "session".into(),
        token: "token".into(),
    }
}

fn session_description(version: u8, key: &[u8], mode: &str) -> String {
    let bytes = serde_json::to_string(key).expect("key serializes");
    format!(
        r#"{{"op":4,"seq":2,"d":{{"mode":"{mode}","secret_key":{bytes},"audio_codec":"opus","dave_protocol_version":{version}}}}}"#
    )
}

/// The server agreeing that our resume worked: the proof that the session is still ours.
fn resumed() -> Inbound {
    text(r#"{"op":9,"d":null}"#)
}

/// Someone else leaving the channel: news for the roster, nothing about our own session.
fn peer_left() -> Inbound {
    text(r#"{"op":13,"d":{"user_id":"9"}}"#)
}

/// Handshake plus select-protocol: the connection as it stands before any session description.
async fn attached(incoming: Vec<Inbound>) -> Connection<MockSocket> {
    attached_with(incoming, VoiceConfig::default()).await
}

async fn attached_with(incoming: Vec<Inbound>, config: VoiceConfig) -> Connection<MockSocket> {
    let mut frames = hello_frames();
    frames.extend(incoming);
    let gateway =
        Gateway::establish(MockSocket::new(frames), &target(), Intro::Identify, config.clone())
            .await
            .expect("handshake");
    let udp = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    Connection::attach(gateway, &target(), fake_path(udp), config).await.expect("attach")
}

/// The transport a mock cannot discover: READY's ssrc and an address we made up.
fn fake_path(udp: UdpSocket) -> Path {
    media_path(udp, "1.2.3.4:5004".parse().unwrap(), 7)
}

fn media_path(udp: UdpSocket, local: SocketAddr, ssrc: u32) -> Path {
    Path { mode: XCHACHA_MODE.to_owned(), ssrc, udp, local }
}

/// The `speaking` value of every op-5 frame written to a gateway, in order.
fn announced(sent: &[String]) -> Vec<u64> {
    sent.iter()
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .filter(|frame| frame["op"] == 5)
        .map(|frame| frame["d"]["speaking"].as_u64().expect("speaking is a number"))
        .collect()
}

#[path = "connect_active.rs"]
mod active;
#[path = "connect_budget.rs"]
mod budget;
#[path = "connect_harness.rs"]
mod harness;
#[path = "connect_measure.rs"]
mod measure;
#[path = "connect_negotiate.rs"]
mod negotiate;
#[path = "connect_pacing.rs"]
mod pacing;
#[path = "connect_reopen.rs"]
mod reopen;
#[path = "connect_scripts.rs"]
mod scripts;
#[path = "connect_speaking.rs"]
mod speaking;
