use serde::{Deserialize, Serialize};

use super::client::GatewayError;
use super::socket::Socket;

#[derive(Serialize)]
pub(crate) struct Frame<T: Serialize> {
    pub(crate) op: u8,
    pub(crate) d: T,
}

#[derive(Deserialize)]
pub(crate) struct RawFrame {
    pub(crate) op: u8,
    #[serde(default)]
    pub(crate) seq: Option<u64>,
    #[serde(default)]
    pub(crate) d: Option<serde_json::Value>,
}

#[derive(Debug)]
pub enum VoiceEvent {
    Json { op: u8, d: Option<serde_json::Value> },
    Dave { op: u8, body: Vec<u8> },
}

pub(crate) fn split_dave_frame(frame: &[u8]) -> Result<(u16, u8, Vec<u8>), GatewayError> {
    if frame.len() < 3 {
        return Err(GatewayError::Protocol(format!(
            "short DAVE frame: {} bytes, need >= 3",
            frame.len()
        )));
    }
    let seq = u16::from_be_bytes(frame[0..2].try_into().unwrap_or([0, 0]));
    Ok((seq, frame[2], frame[3..].to_vec()))
}

#[derive(Debug)]
pub(crate) enum Reply {
    Text(String),
    Binary(Vec<u8>),
}

impl Reply {
    pub(crate) async fn send<S: Socket>(self, socket: &mut S) -> Result<(), GatewayError> {
        match self {
            Reply::Text(text) => socket.send_text(text).await,
            Reply::Binary(bytes) => socket.send_binary(bytes).await,
        }
    }
}

pub(crate) fn json_reply<T: Serialize>(op: u8, data: &T) -> Result<Reply, GatewayError> {
    Ok(Reply::Text(frame_text(op, data)?))
}

pub(crate) fn frame_text<T: Serialize>(op: u8, data: &T) -> Result<String, GatewayError> {
    serde_json::to_string(&Frame { op, d: data }).map_err(GatewayError::from)
}

pub(crate) fn binary_reply(op: u8, payload: &[u8]) -> Reply {
    let mut frame = Vec::with_capacity(payload.len() + 1);
    frame.push(op);
    frame.extend_from_slice(payload);
    Reply::Binary(frame)
}
