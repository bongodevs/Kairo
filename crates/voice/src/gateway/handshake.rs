use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::DAVE_PROTOCOL_VERSION;
use super::client::{GatewayError, Ready, VoiceTarget};
use super::heartbeat::Heartbeat;
use super::socket::{Inbound, Socket};
use super::wire::{Frame, RawFrame, VoiceEvent, split_dave_frame};
use crate::ids::{ChannelId, GuildId, UserId};
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const FALLBACK_HEARTBEAT: Duration = Duration::from_secs(60);

const OP_IDENTIFY: u8 = 0;
const OP_READY: u8 = 2;
const OP_RESUME: u8 = 7;
const OP_HELLO: u8 = 8;
const OP_RESUMED: u8 = 9;
const OP_HEARTBEAT_ACK: u8 = 6;

#[derive(Serialize)]
struct Identify {
    server_id: GuildId,
    user_id: UserId,
    session_id: String,
    token: String,
    video: bool,
    channel_id: ChannelId,
    max_dave_protocol_version: u16,
}

#[derive(Serialize)]
struct Resume {
    server_id: GuildId,
    session_id: String,
    token: String,
    video: bool,
    seq_ack: i64,
}

#[derive(Deserialize)]
struct Hello {
    heartbeat_interval: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Intro {
    Identify,
    Resume(i64),
}

#[derive(Debug)]
pub(crate) struct Handshake {
    pub(crate) opening: Opening,
    pub(crate) heartbeat: Heartbeat,
    pub(crate) pending: VecDeque<VoiceEvent>,
}

#[derive(Debug, Clone)]
pub(crate) enum Opening {
    Ready(Ready),
    Resumed,
}

pub(crate) async fn handshake<S: Socket>(
    socket: &mut S,
    target: &VoiceTarget,
    intro: Intro,
) -> Result<Handshake, GatewayError> {
    socket.send_text(intro_frame(target, intro)?).await?;
    await_opening(socket, intro).await
}

fn intro_frame(target: &VoiceTarget, intro: Intro) -> Result<String, GatewayError> {
    let server_id = target.guild;
    match intro {
        Intro::Identify => serde_json::to_string(&Frame {
            op: OP_IDENTIFY,
            d: Identify {
                server_id,
                user_id: target.user,
                session_id: target.session_id.clone(),
                token: target.token.clone(),
                video: true,
                channel_id: target.channel,
                max_dave_protocol_version: DAVE_PROTOCOL_VERSION,
            },
        }),
        Intro::Resume(seq_ack) => serde_json::to_string(&Frame {
            op: OP_RESUME,
            d: Resume {
                server_id,
                session_id: target.session_id.clone(),
                token: target.token.clone(),
                video: true,
                seq_ack,
            },
        }),
    }
    .map_err(GatewayError::from)
}

async fn await_opening<S: Socket>(socket: &mut S, intro: Intro) -> Result<Handshake, GatewayError> {
    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    let mut heartbeat = None;
    let mut last_seq = match intro {
        Intro::Resume(seq) => u64::try_from(seq).ok(),
        Intro::Identify => None,
    };
    let mut pending = VecDeque::new();
    loop {
        let beat = heartbeat.as_ref().map(Heartbeat::deadline);
        let inbound = tokio::select! {
            () = tokio::time::sleep_until(deadline) => return Err(GatewayError::Timeout),
            () = wake_at(beat) => {
                if let Some(heartbeat) = heartbeat.as_mut() {
                    heartbeat.send(socket).await?;
                }
                continue;
            }
            result = socket.recv() => result?,
        };
        let Some(inbound) = inbound else {
            return Err(GatewayError::Closed);
        };
        let text = match inbound {
            Inbound::Binary(frame) => {
                let (seq, op, body) = split_dave_frame(&frame)?;
                observe(&mut heartbeat, &mut last_seq, u64::from(seq));
                pending.push_back(VoiceEvent::Dave { op, body });
                continue;
            }
            Inbound::Text(text) => text,
            Inbound::Closed(code, reason) => {
                return Err(GatewayError::CloseFrame { code, reason });
            }
        };
        let frame: RawFrame =
            serde_json::from_str(&text).map_err(|err| GatewayError::Protocol(err.to_string()))?;
        if let Some(seq) = frame.seq {
            observe(&mut heartbeat, &mut last_seq, seq);
        }
        match frame.op {
            OP_HELLO => {
                let hello: Hello = deserialize_data(&frame, "hello")?;
                let interval = Duration::from_millis(hello.heartbeat_interval as u64);
                match heartbeat.as_mut() {
                    Some(heartbeat) => heartbeat.set_interval(interval),
                    None => heartbeat = Some(Heartbeat::new(interval, last_seq)),
                }
            }
            OP_READY => {
                let ready: Ready = deserialize_data(&frame, "ready")?;
                return Ok(Handshake {
                    opening: Opening::Ready(ready),
                    heartbeat: heartbeat
                        .unwrap_or_else(|| Heartbeat::new(FALLBACK_HEARTBEAT, last_seq)),
                    pending,
                });
            }
            OP_RESUMED => {
                pending.push_back(VoiceEvent::Json { op: OP_RESUMED, d: frame.d });
                return Ok(Handshake {
                    opening: Opening::Resumed,
                    heartbeat: heartbeat
                        .unwrap_or_else(|| Heartbeat::new(FALLBACK_HEARTBEAT, last_seq)),
                    pending,
                });
            }
            OP_HEARTBEAT_ACK => {
                if let Some(heartbeat) = heartbeat.as_mut() {
                    heartbeat.acknowledged();
                }
            }
            op => pending.push_back(VoiceEvent::Json { op, d: frame.d }),
        }
    }
}

fn observe(heartbeat: &mut Option<Heartbeat>, last_seq: &mut Option<u64>, seq: u64) {
    *last_seq = Some(seq);
    if let Some(heartbeat) = heartbeat {
        heartbeat.observe(seq);
    }
}

async fn wake_at(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn deserialize_data<T: serde::de::DeserializeOwned>(
    frame: &RawFrame,
    what: &str,
) -> Result<T, GatewayError> {
    let value =
        frame.d.as_ref().ok_or_else(|| GatewayError::Protocol(format!("missing d in {what}")))?;
    serde_json::from_value(value.clone()).map_err(|err| GatewayError::Protocol(err.to_string()))
}
