use serde_json::Value;
use tokio::net::UdpSocket;

use super::disconnect::{Disconnect, Retry};
use super::error::ConnectionError;
use super::path::key_bytes;
use super::wire::{OP_RESUMED, OP_SESSION_DESCRIPTION, OP_SPEAKING, OP_VIDEO, SpeakingUpdate};
use crate::crypto::TransportCipher;
use crate::dave::{DaveHandler, DaveOut};
use crate::gateway::{Handled, Pump, Reply, VoiceEvent, VoiceTarget, binary_reply, json_reply};
use crate::sender::{Command, FrameSource, Media, SPEAKING_MICROPHONE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    AwaitingSession,
    Ready,
}

pub(crate) struct Session {
    pub(crate) dave: DaveHandler,
    pub(super) mode: String,
    pub(crate) cipher: Option<TransportCipher>,
    pub(super) phase: Phase,
    pub(super) ssrc: u32,
    pub(super) mask: u8,
    pub(super) send_stop: bool,
    pub(super) retry: Retry,
    mask_dirty: bool,
}

impl Session {
    pub(super) fn new(target: &VoiceTarget, mode: String, ssrc: u32, send_stop: bool) -> Self {
        let mut retry = Retry::default();
        retry.saw_ready();
        Self {
            dave: DaveHandler::new(target.user.0, target.channel.0),
            mode,
            cipher: None,
            phase: Phase::AwaitingSession,
            ssrc,
            mask: 0,
            send_stop,
            retry,
            mask_dirty: false,
        }
    }

    pub(crate) fn ready_to_send(&self) -> bool {
        self.phase == Phase::Ready && (self.dave.protocol_version() == 0 || self.dave.can_encrypt())
    }

    pub(super) fn on_pump<S: FrameSource>(
        &mut self,
        pump: Pump<Command>,
        source: &mut S,
        udp: &UdpSocket,
        media: &mut Media,
    ) -> Result<Handled, ConnectionError> {
        let mut end = None;
        let replies = match pump {
            Pump::Control(Command::Speak(mask)) => self.set_mask(mask)?,
            Pump::Control(Command::Produce) => {
                media.pacing.arm();
                Vec::new()
            }
            Pump::Control(Command::Close { code, reason }) => {
                end = Some(Disconnect::local(code, reason));
                Vec::new()
            }
            Pump::Tick => {
                let (keep_pacing, spoke) = self.tick(source, udp, media)?;
                if keep_pacing {
                    media.pacing.advance();
                    if spoke && self.mask == 0 {
                        self.set_mask(SPEAKING_MICROPHONE)?
                    } else {
                        Vec::new()
                    }
                } else {
                    media.pacing.stop();
                    self.set_mask(0)?
                }
            }
            Pump::Frame(event) => self.on_event(event)?,
        };
        Ok(Handled { replies, wake: media.pacing.deadline(), end })
    }

    fn set_mask(&mut self, mask: u8) -> Result<Vec<Reply>, ConnectionError> {
        if mask == self.mask {
            return Ok(Vec::new());
        }
        self.mask = mask;
        self.mask_dirty = false;
        if mask == 0 && !self.send_stop {
            return Ok(Vec::new());
        }
        self.announce().map(|reply| vec![reply])
    }

    fn on_event(&mut self, event: VoiceEvent) -> Result<Vec<Reply>, ConnectionError> {
        if matches!(&event, VoiceEvent::Json { op: OP_SESSION_DESCRIPTION | OP_RESUMED, .. }) {
            self.retry.described();
        }
        let respeak =
            matches!(&event, VoiceEvent::Json { op: OP_SESSION_DESCRIPTION | OP_VIDEO, .. });
        if let VoiceEvent::Json { op: OP_SESSION_DESCRIPTION, d } = &event {
            self.apply(d.as_ref())?;
        }
        let mut replies: Vec<Reply> = self
            .dave
            .handle(&event)
            .into_iter()
            .map(|out| match out {
                DaveOut::Json { op, data } => json_reply(op, &data).map_err(Into::into),
                DaveOut::Binary { op, payload } => Ok(binary_reply(op, &payload)),
            })
            .collect::<Result<_, ConnectionError>>()?;
        if respeak && self.mask != 0 {
            replies.push(self.announce()?);
            self.mask_dirty = false;
        }
        Ok(replies)
    }

    fn announce(&self) -> Result<Reply, ConnectionError> {
        Ok(json_reply(
            OP_SPEAKING,
            &SpeakingUpdate { speaking: self.mask, delay: 0, ssrc: self.ssrc },
        )?)
    }

    pub(super) fn while_offline(
        &mut self,
        command: Command,
        media: &mut Media,
    ) -> Option<Disconnect> {
        match command {
            Command::Speak(mask) => {
                if mask != self.mask {
                    self.mask = mask;
                    self.mask_dirty = mask != 0 || self.send_stop;
                }
                None
            }
            Command::Produce => {
                media.pacing.arm();
                None
            }
            Command::Close { code, reason } => Some(Disconnect::local(code, reason)),
        }
    }

    pub(super) fn resumed_announcement(&mut self) -> Result<Option<Reply>, ConnectionError> {
        if !self.mask_dirty {
            return Ok(None);
        }
        self.mask_dirty = false;
        self.announce().map(Some)
    }

    fn apply(&mut self, d: Option<&Value>) -> Result<(), ConnectionError> {
        let data =
            d.ok_or_else(|| ConnectionError::SessionDescription("missing payload".into()))?;
        let mode = data
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| ConnectionError::SessionDescription("no mode field".into()))?;
        if mode != self.mode {
            return Err(ConnectionError::ModeMismatch {
                server: mode.to_owned(),
                wanted: self.mode.clone(),
            });
        }
        let key = key_bytes(data)?;
        self.cipher = Some(
            TransportCipher::new(mode, &key)
                .map_err(|err| ConnectionError::SessionDescription(err.to_string()))?,
        );
        if let Some(codec) = data.get("audio_codec").and_then(Value::as_str)
            && codec != "opus"
        {
            return Err(ConnectionError::SessionDescription(format!(
                "negotiated codec {codec:?}, we only produce opus"
            )));
        }
        self.phase = Phase::Ready;
        tracing::debug!(mode, "voice session described");
        Ok(())
    }
}
