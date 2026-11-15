use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::handshake::{Handshake, Intro, Opening, handshake};
use super::heartbeat::Heartbeat;
use super::socket::{Inbound, Socket, open_socket};
use super::wire::{RawFrame, VoiceEvent, frame_text, split_dave_frame};
use crate::config::VoiceConfig;
use crate::ids::{ChannelId, GuildId, UserId};
pub type GatewaySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_MAX_MS: u64 = 30_000;
const OP_VOICE_BACKEND_VERSION: u8 = 16;
const OP_HEARTBEAT_ACK: u8 = 6;
const OP_MEDIA_SINK_WANTS: u8 = 15;

#[derive(Debug, Clone)]
pub struct VoiceTarget {
    pub endpoint: String,
    pub guild: GuildId,
    pub channel: ChannelId,
    pub user: UserId,
    pub session_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Ready {
    pub ip: String,
    pub port: u16,
    pub ssrc: u32,
    #[serde(default)]
    pub modes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("connect: {0}")]
    Connect(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("encode: {source}")]
    Encode {
        #[from]
        source: serde_json::Error,
    },
    #[error("handshake timed out")]
    Timeout,
    #[error("socket closed before ready")]
    Closed,
    #[error("socket closed before ready: {code} {reason}")]
    CloseFrame { code: u16, reason: String },
}

pub(crate) struct Gateway<S> {
    socket: S,
    opening: Opening,
    heartbeat: Heartbeat,
    pending: VecDeque<VoiceEvent>,
}

impl Gateway<GatewaySocket> {
    pub(crate) async fn connect(
        target: &VoiceTarget,
        intro: Intro,
        config: &VoiceConfig,
    ) -> Result<Gateway<GatewaySocket>, GatewayError> {
        let socket = open_socket(&target.endpoint, config).await?;
        Self::establish(socket, target, intro, config.clone()).await
    }
}

impl<S> Gateway<S> {
    pub(crate) fn opening(&self) -> &Opening {
        &self.opening
    }

    pub(crate) fn ready(&self) -> Option<&Ready> {
        match &self.opening {
            Opening::Ready(ready) => Some(ready),
            Opening::Resumed => None,
        }
    }
}

impl<S: Socket> Gateway<S> {
    pub(crate) async fn establish(
        mut socket: S,
        target: &VoiceTarget,
        intro: Intro,
        config: VoiceConfig,
    ) -> Result<Gateway<S>, GatewayError> {
        let handshake = handshake(&mut socket, target, intro).await?;
        if matches!(handshake.opening, Opening::Ready(_)) {
            socket
                .send_text(frame_text(
                    OP_MEDIA_SINK_WANTS,
                    &json!({ "any": if config.deafened { 0 } else { 100 } }),
                )?)
                .await?;
            socket.send_text(frame_text(OP_VOICE_BACKEND_VERSION, &json!({}))?).await?;
        }
        Ok(Self::from_handshake(socket, handshake))
    }

    fn from_handshake(socket: S, h: Handshake) -> Self {
        Self { socket, opening: h.opening, heartbeat: h.heartbeat, pending: h.pending }
    }

    pub(crate) async fn drive_while<F, T, E>(&mut self, future: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
        E: From<GatewayError>,
    {
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => return result,
                () = tokio::time::sleep_until(self.heartbeat.deadline()) => {
                    self.heartbeat.send(&mut self.socket).await?;
                }
                inbound = self.socket.recv() => self.queue(inbound?).map_err(E::from)?,
            }
        }
    }

    fn queue(&mut self, inbound: Option<Inbound>) -> Result<(), GatewayError> {
        let Some(inbound) = inbound else {
            return Err(GatewayError::Closed);
        };
        match inbound {
            Inbound::Closed(code, reason) => Err(GatewayError::CloseFrame { code, reason }),
            Inbound::Binary(frame) => {
                let (seq, op, body) = split_dave_frame(&frame)?;
                self.heartbeat.observe(u64::from(seq));
                self.pending.push_back(VoiceEvent::Dave { op, body });
                Ok(())
            }
            Inbound::Text(text) => {
                let frame: RawFrame = serde_json::from_str(&text)
                    .map_err(|err| GatewayError::Protocol(err.to_string()))?;
                if let Some(seq) = frame.seq {
                    self.heartbeat.observe(seq);
                }
                if frame.op == OP_HEARTBEAT_ACK {
                    self.heartbeat.acknowledged();
                } else {
                    self.pending.push_back(VoiceEvent::Json { op: frame.op, d: frame.d });
                }
                Ok(())
            }
        }
    }

    pub(crate) fn split(self) -> (S, Heartbeat, VecDeque<VoiceEvent>) {
        (self.socket, self.heartbeat, self.pending)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CloseAction {
    Resume,
    Reidentify,
    Fatal,
}

pub(crate) fn close_action(code: u16) -> CloseAction {
    match code {
        1001 | 1006 | 4000 | 4001 | 4002 | 4003 | 4005 | 4009 | 4012 | 4015 | 4016 | 4020
        | 4900 => CloseAction::Resume,
        4006 => CloseAction::Reidentify,
        _ => CloseAction::Fatal,
    }
}

pub(crate) fn backoff_delay(attempt: u32, r: u64) -> Duration {
    let full =
        BACKOFF_BASE_MS.checked_shl(attempt.min(16)).unwrap_or(BACKOFF_MAX_MS).min(BACKOFF_MAX_MS);
    let half = full / 2;
    Duration::from_millis(half + r % (half + 1))
}

pub(crate) fn reconnect_delay(attempt: u32) -> Duration {
    backoff_delay(attempt, getrandom::u64().unwrap_or(0))
}

#[cfg(test)]
#[path = "../../tests/gateway.rs"]
mod tests;
