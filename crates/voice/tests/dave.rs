use serde_json::{Value, json};

use super::fields::split_u16;
use super::*;
use crate::gateway::VoiceEvent;
fn json_event(op: u8, data: Value) -> VoiceEvent {
    VoiceEvent::Json { op, d: Some(data) }
}

#[test]
fn session_description_at_v1_emits_a_key_package() {
    let mut dave = DaveHandler::new(1, 2);
    let outs = dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));
    assert!(
        matches!(&outs[..], [DaveOut::Binary { op: 26, payload }] if !payload.is_empty()),
        "{outs:?}"
    );
    assert_eq!(dave.protocol_version, 1);
    assert!(dave.session.is_some());
}

#[test]
fn a_media_frame_before_the_group_has_keys_is_refused_not_leaked() {
    let mut dave = DaveHandler::new(1, 2);
    dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));
    assert!(dave.session.is_some(), "a key package was made");
    assert!(!dave.can_encrypt(), "no key ratchet yet");
    let mut out = vec![0u8; 8];
    assert!(
        dave.encrypt_opus_into(&[0xf8, 0xff, 0xfe], &mut out).is_none(),
        "the sender must drop the frame, never send it in the clear"
    );
}

#[test]
fn session_description_at_v0_stays_transport_only() {
    let mut dave = DaveHandler::new(1, 2);
    let outs =
        dave.handle(&json_event(OP_SESSION_DESCRIPTION, json!({ "dave_protocol_version": 0 })));
    assert!(outs.is_empty(), "{outs:?}");
    assert!(dave.downgraded);
    assert!(!dave.is_active());
}

#[test]
fn prepare_epoch_one_creates_a_key_package() {
    let mut dave = DaveHandler::new(1, 2);
    let outs = dave.handle(&json_event(
        OP_DAVE_PREPARE_EPOCH,
        json!({ "transition_id": 0, "protocol_version": 1, "epoch": 1 }),
    ));
    assert!(matches!(&outs[..], [DaveOut::Binary { op: 26, .. }]));
}

#[test]
fn prepare_epoch_above_one_is_ignored() {
    let mut dave = DaveHandler::new(1, 2);
    let outs = dave.handle(&json_event(
        OP_DAVE_PREPARE_EPOCH,
        json!({ "transition_id": 4, "protocol_version": 1, "epoch": 2 }),
    ));
    assert!(outs.is_empty());
}

#[test]
fn initial_transition_is_applied_at_prepare_and_execute_is_idempotent() {
    let mut dave = DaveHandler::new(1, 2);
    dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));
    let outs = dave.handle(&json_event(
        OP_DAVE_PREPARE_TRANSITION,
        json!({ "transition_id": 0, "protocol_version": 1 }),
    ));
    assert!(outs.is_empty(), "transition zero does not announce ready");
    assert_eq!(dave.protocol_version, 1);
    assert!(dave.pending_transitions.is_empty());

    let outs = dave.handle(&json_event(OP_DAVE_EXECUTE_TRANSITION, json!({ "transition_id": 0 })));
    assert!(outs.is_empty());
    assert_eq!(dave.protocol_version, 1);
    assert!(dave.pending_transitions.is_empty());
}

#[test]
fn initial_transition_to_zero_downgrades_at_prepare() {
    let mut dave = DaveHandler::new(1, 2);
    dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));
    let outs = dave.handle(&json_event(
        OP_DAVE_PREPARE_TRANSITION,
        json!({ "transition_id": 0, "protocol_version": 0 }),
    ));
    assert!(outs.is_empty());
    assert_eq!(dave.protocol_version, 0);
    assert!(dave.downgraded);
    assert!(dave.pending_transitions.is_empty());

    assert!(
        dave.handle(&json_event(OP_DAVE_EXECUTE_TRANSITION, json!({ "transition_id": 0 }),))
            .is_empty()
    );
    assert_eq!(dave.protocol_version, 0);
    assert!(dave.downgraded);
}

#[test]
fn downgrade_transition_announces_ready_and_pends_execution() {
    let mut dave = DaveHandler::new(1, 2);
    let outs = dave.handle(&json_event(
        OP_DAVE_PREPARE_TRANSITION,
        json!({ "transition_id": 7, "protocol_version": 0 }),
    ));
    assert!(
        matches!(&outs[..], [DaveOut::Json { op: 23, data }] if data["transition_id"] == 7),
        "{outs:?}"
    );
    assert_eq!(dave.pending_transitions.get(&7), Some(&0));
}

#[test]
fn execute_transition_applies_the_pended_version() {
    let mut dave = DaveHandler::new(1, 2);
    dave.handle(&json_event(
        OP_DAVE_PREPARE_TRANSITION,
        json!({ "transition_id": 9, "protocol_version": 0 }),
    ));
    let outs = dave.handle(&json_event(OP_DAVE_EXECUTE_TRANSITION, json!({ "transition_id": 9 })));
    assert!(outs.is_empty());
    assert_eq!(dave.protocol_version, 0);
    assert!(dave.pending_transitions.is_empty());
}

#[test]
fn a_reinitialized_session_stays_downgraded_until_the_upgrade_executes() {
    let mut dave = DaveHandler::new(1, 2);
    dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));
    dave.handle(&json_event(
        OP_DAVE_PREPARE_TRANSITION,
        json!({ "transition_id": 4, "protocol_version": 0 }),
    ));
    dave.handle(&json_event(OP_DAVE_EXECUTE_TRANSITION, json!({ "transition_id": 4 })));
    assert!(dave.downgraded, "executing the downgrade flips the flag");

    // Reinitialising keeps the old keys, so readiness alone must not restart
    // encrypted sending: the upgrade transition is what ends passthrough.
    dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));
    assert!(dave.downgraded);
    assert_eq!(dave.protocol_version, 1);
    assert!(!dave.is_active(), "still transport-only before the transition");

    dave.handle(&json_event(
        OP_DAVE_PREPARE_TRANSITION,
        json!({ "transition_id": 5, "protocol_version": 1 }),
    ));
    assert!(dave.downgraded, "preparing alone does not switch the key");
    dave.handle(&json_event(OP_DAVE_EXECUTE_TRANSITION, json!({ "transition_id": 5 })));
    assert!(!dave.downgraded, "executing the upgrade ends transport-only");
}

#[test]
fn a_commit_ignored_by_the_session_is_not_announced_as_ready() {
    let mut dave = DaveHandler::new(1, 2);
    dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));
    // No group yet, so the commit is ignored rather than failing or
    // producing a transition-ready frame.
    let outs = dave.handle(&VoiceEvent::Dave {
        op: OP_MLS_ANNOUNCE_COMMIT_TRANSITION,
        body: vec![0, 5, 0xAA],
    });
    assert!(outs.is_empty(), "{outs:?}");
}

#[test]
fn clients_connect_tracks_ids_without_crashing_on_shape() {
    let mut dave = DaveHandler::new(1, 2);
    dave.handle(&json_event(OP_CLIENTS_CONNECT, json!({ "user_ids": ["42", "not-a-snowflake"] })));
    assert!(dave.recognized.contains(&42), "user_ids is the shape");
    // An unexpected payload must not panic or poison the set.
    dave.handle(&json_event(OP_CLIENTS_CONNECT, json!({ "unexpected": true })));
    assert!(dave.recognized.contains(&1));
    let expected = dave.recognized_ids();
    assert!(expected.contains(&1), "our own credential is always expected");
    assert!(expected.contains(&42), "the op-11 roster is validated");
}

#[test]
fn client_disconnect_removes_the_user() {
    let mut dave = DaveHandler::new(1, 2);
    dave.recognized.insert(77);
    dave.handle(&json_event(OP_CLIENT_DISCONNECT, json!({ "user_id": "77" })));
    assert!(!dave.recognized.contains(&77));
}

#[test]
fn binary_frames_before_a_session_are_dropped_not_panicked() {
    let mut dave = DaveHandler::new(1, 2);
    for op in [OP_MLS_EXTERNAL_SENDER, OP_MLS_PROPOSALS, OP_MLS_WELCOME] {
        assert!(dave.handle(&VoiceEvent::Dave { op, body: vec![0, 1, 2] }).is_empty(), "op {op}");
    }
}

#[test]
fn split_u16_needs_a_two_byte_header() {
    assert!(split_u16(&[0x01]).is_none());
    assert_eq!(split_u16(&[0x12, 0x34, 0xAA]), Some((0x1234, &[0xAA][..])));
}
