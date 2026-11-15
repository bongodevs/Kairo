use std::collections::VecDeque;

use tokio::sync::mpsc;

use super::client::GatewayError;
use super::heartbeat::Heartbeat;
use super::socket::{Inbound, Socket};
use super::wire::{RawFrame, Reply, VoiceEvent, split_dave_frame};
use crate::connection::disconnect::{CLOSE_ABNORMAL, Disconnect};
const OP_HEARTBEAT_ACK: u8 = 6;

pub(crate) enum Pump<C> {
    Frame(VoiceEvent),
    Control(C),
    Tick,
}

#[derive(Debug, Default)]
pub(crate) struct Handled {
    pub(crate) replies: Vec<Reply>,
    pub(crate) wake: Option<tokio::time::Instant>,
    pub(crate) end: Option<Disconnect>,
}

impl Handled {
    #[cfg(test)]
    pub(crate) fn replies(replies: Vec<Reply>) -> Self {
        Self { replies, ..Self::default() }
    }
}

#[cfg(test)]
pub(crate) async fn heartbeat_loop<S: Socket, F, E, C>(
    socket: &mut S,
    hb: &mut Heartbeat,
    controls: &mut mpsc::Receiver<C>,
    on_pump: F,
) -> Result<Disconnect, E>
where
    F: FnMut(Pump<C>) -> Result<Handled, E>,
    E: From<GatewayError>,
{
    let mut pending = VecDeque::new();
    heartbeat_loop_with_pending(socket, hb, controls, &mut pending, on_pump).await
}

pub(crate) async fn heartbeat_loop_with_pending<S: Socket, F, E, C>(
    socket: &mut S,
    hb: &mut Heartbeat,
    controls: &mut mpsc::Receiver<C>,
    pending: &mut VecDeque<VoiceEvent>,
    mut on_pump: F,
) -> Result<Disconnect, E>
where
    F: FnMut(Pump<C>) -> Result<Handled, E>,
    E: From<GatewayError>,
{
    let mut wake = None;
    while let Some(event) = pending.pop_front() {
        if let Some(close) = apply(socket, &mut on_pump, Pump::Frame(event), &mut wake).await? {
            return Ok(close);
        }
    }
    loop {
        let inbound = tokio::select! {
            biased;
            Some(control) = controls.recv() => {
                if let Some(close) = apply(socket, &mut on_pump, Pump::Control(control), &mut wake).await? {
                    return Ok(close);
                }
                continue;
            }
            () = wake_at(wake) => {
                if let Some(close) = apply(socket, &mut on_pump, Pump::Tick, &mut wake).await? {
                    return Ok(close);
                }
                continue;
            }
            result = tokio::time::timeout_at(hb.deadline(), socket.recv()) => result,
        };
        let inbound = match inbound {
            Err(_elapsed) => {
                hb.send(socket).await?;
                continue;
            }
            Ok(result) => result?,
        };
        let Some(inbound) = inbound else {
            return Ok(Disconnect::abnormal("the gateway stopped without a close frame"));
        };
        match inbound {
            Inbound::Closed(code, reason) => return Ok(Disconnect::remote(code, reason)),
            Inbound::Binary(frame) => {
                let (seq, op, body) = split_dave_frame(&frame)?;
                hb.observe(u64::from(seq));
                if let Some(close) = apply(
                    socket,
                    &mut on_pump,
                    Pump::Frame(VoiceEvent::Dave { op, body }),
                    &mut wake,
                )
                .await?
                {
                    return Ok(close);
                }
            }
            Inbound::Text(text) => {
                let frame: RawFrame = serde_json::from_str(&text)
                    .map_err(|err| GatewayError::Protocol(err.to_string()))?;
                if let Some(seq) = frame.seq {
                    hb.observe(seq);
                }
                match frame.op {
                    OP_HEARTBEAT_ACK => hb.acknowledged(),
                    other => {
                        let event = VoiceEvent::Json { op: other, d: frame.d };
                        if let Some(close) =
                            apply(socket, &mut on_pump, Pump::Frame(event), &mut wake).await?
                        {
                            return Ok(close);
                        }
                    }
                }
            }
        }
    }
}

async fn apply<S: Socket, F, E, C>(
    socket: &mut S,
    on_pump: &mut F,
    pump: Pump<C>,
    wake: &mut Option<tokio::time::Instant>,
) -> Result<Option<Disconnect>, E>
where
    F: FnMut(Pump<C>) -> Result<Handled, E>,
    E: From<GatewayError>,
{
    let Handled { replies, wake: next, end } = on_pump(pump)?;
    flush(socket, replies).await?;
    *wake = next;
    let Some(close) = end else {
        return Ok(None);
    };
    if close.code != CLOSE_ABNORMAL {
        socket.close(close.code, close.reason.clone()).await;
    }
    Ok(Some(close))
}

async fn wake_at(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn flush<S: Socket, E: From<GatewayError>>(
    socket: &mut S,
    replies: Vec<Reply>,
) -> Result<(), E> {
    for reply in replies {
        reply.send(socket).await?;
    }
    Ok(())
}
