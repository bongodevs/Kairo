use std::collections::{HashMap, HashSet};

use davey::DaveSession;
use serde_json::Value;
mod fields;
mod handler;
mod session;

const OP_SESSION_DESCRIPTION: u8 = 4;
const OP_CLIENTS_CONNECT: u8 = 11;
const OP_CLIENT_DISCONNECT: u8 = 13;
const OP_DAVE_PREPARE_TRANSITION: u8 = 21;
const OP_DAVE_EXECUTE_TRANSITION: u8 = 22;
const OP_DAVE_TRANSITION_READY: u8 = 23;
const OP_DAVE_PREPARE_EPOCH: u8 = 24;
const OP_MLS_EXTERNAL_SENDER: u8 = 25;
const OP_MLS_KEY_PACKAGE: u8 = 26;
const OP_MLS_PROPOSALS: u8 = 27;
const OP_MLS_COMMIT_WELCOME: u8 = 28;
const OP_MLS_ANNOUNCE_COMMIT_TRANSITION: u8 = 29;
const OP_MLS_WELCOME: u8 = 30;
const OP_MLS_INVALID_COMMIT_WELCOME: u8 = 31;

const PASSTHROUGH_EXPIRY: u32 = 10;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DaveOut {
    Json { op: u8, data: Value },
    Binary { op: u8, payload: Vec<u8> },
}

pub(crate) struct DaveHandler {
    user_id: u64,
    channel_id: u64,
    protocol_version: u16,
    session: Option<DaveSession>,
    pending_transitions: HashMap<u16, u16>,
    recognized: HashSet<u64>,
    downgraded: bool,
}

#[cfg(test)]
#[path = "../../tests/dave.rs"]
mod tests;
