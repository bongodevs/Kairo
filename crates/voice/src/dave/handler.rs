use std::collections::{HashMap, HashSet};

use davey::{Codec, DaveSession, MediaType};
use serde_json::Value;

use super::fields::{field_u16, snowflake, track_clients, track_one};
use super::{
    DaveOut, OP_CLIENT_DISCONNECT, OP_CLIENTS_CONNECT, OP_DAVE_EXECUTE_TRANSITION,
    OP_DAVE_PREPARE_EPOCH, OP_DAVE_PREPARE_TRANSITION, OP_MLS_ANNOUNCE_COMMIT_TRANSITION,
    OP_MLS_EXTERNAL_SENDER, OP_MLS_PROPOSALS, OP_MLS_WELCOME, OP_SESSION_DESCRIPTION,
};
use crate::gateway::VoiceEvent;

impl super::DaveHandler {
    pub(crate) fn new(user_id: u64, channel_id: u64) -> Self {
        let mut recognized = HashSet::new();
        recognized.insert(user_id);
        Self {
            user_id,
            channel_id,
            protocol_version: 0,
            session: None,
            pending_transitions: HashMap::new(),
            recognized,
            downgraded: false,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.protocol_version != 0 && !self.downgraded && self.ready()
    }

    fn ready(&self) -> bool {
        self.session.as_ref().is_some_and(DaveSession::is_ready)
    }

    pub(crate) fn can_encrypt(&self) -> bool {
        self.session.as_ref().is_some_and(DaveSession::can_encrypt)
    }

    pub(crate) fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    pub(crate) fn encrypt_opus_into(&mut self, packet: &[u8], out: &mut Vec<u8>) -> Option<()> {
        let session = self.session.as_mut()?;
        session.encrypt_into(MediaType::AUDIO, Codec::OPUS, packet, out).ok()
    }

    pub(crate) fn handle(&mut self, event: &VoiceEvent) -> Vec<DaveOut> {
        match event {
            VoiceEvent::Json { op, d } => self.handle_json(*op, d.as_ref()),
            VoiceEvent::Dave { op, body } => self.handle_binary(*op, body),
        }
    }

    fn handle_json(&mut self, op: u8, d: Option<&Value>) -> Vec<DaveOut> {
        let empty = Value::Null;
        let d = d.unwrap_or(&empty);
        match op {
            OP_SESSION_DESCRIPTION => self.begin_version(field_u16(d, "dave_protocol_version")),
            OP_DAVE_PREPARE_EPOCH => self.prepare_epoch(d),
            OP_DAVE_PREPARE_TRANSITION => self.prepare_transition(d),
            OP_DAVE_EXECUTE_TRANSITION => self.execute_pending(field_u16(d, "transition_id")),
            OP_CLIENTS_CONNECT => {
                track_clients(d, &mut self.recognized, true);
                Vec::new()
            }
            OP_CLIENT_DISCONNECT => {
                if let Some(user_id) = snowflake(d, "user_id") {
                    track_one(&mut self.recognized, user_id, false);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_binary(&mut self, op: u8, body: &[u8]) -> Vec<DaveOut> {
        match op {
            OP_MLS_EXTERNAL_SENDER => {
                if let Some(session) = self.session.as_mut()
                    && let Err(err) = session.set_external_sender(body)
                {
                    tracing::warn!("DAVE external sender rejected: {err}");
                }
                Vec::new()
            }
            OP_MLS_PROPOSALS => self.process_proposals(body),
            OP_MLS_ANNOUNCE_COMMIT_TRANSITION => self.process_commit(body),
            OP_MLS_WELCOME => self.process_welcome(body),
            // 26, 28, 31 are client->server only.
            _ => Vec::new(),
        }
    }
}
