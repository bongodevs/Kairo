use std::collections::HashSet;

use serde_json::{Value, json};

use super::{DaveOut, OP_DAVE_TRANSITION_READY};

pub(super) fn transition_ready(transition_id: u16) -> DaveOut {
    DaveOut::Json { op: OP_DAVE_TRANSITION_READY, data: json!({ "transition_id": transition_id }) }
}

pub(super) fn split_u16(body: &[u8]) -> Option<(u16, &[u8])> {
    if body.len() < 2 {
        return None;
    }
    let id = u16::from_be_bytes(body[0..2].try_into().ok()?);
    Some((id, &body[2..]))
}

pub(super) fn field_u16(d: &Value, key: &str) -> u16 {
    field_u64(d, key) as u16
}

pub(super) fn field_u64(d: &Value, key: &str) -> u64 {
    d.get(key).and_then(Value::as_u64).unwrap_or(0)
}

pub(super) fn snowflake(d: &Value, key: &str) -> Option<u64> {
    d.get(key).and_then(Value::as_str).and_then(|value| value.parse().ok())
}

pub(super) fn track_clients(d: &Value, recognized: &mut HashSet<u64>, add: bool) {
    if let Some(ids) = d.get("user_ids").and_then(Value::as_array) {
        for id in ids {
            if let Some(user_id) = id.as_str().and_then(|value| value.parse().ok()) {
                track_one(recognized, user_id, add);
            }
        }
    }
}

pub(super) fn track_one(recognized: &mut HashSet<u64>, user_id: u64, add: bool) {
    if add {
        recognized.insert(user_id);
    } else {
        recognized.remove(&user_id);
    }
}
