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

#[test]
fn mutation_test_dave_frame_handler_never_panics_or_hangs() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0
        }
        fn next_usize(&mut self, max: usize) -> usize {
            if max == 0 { 0 } else { (self.next() as usize) % max }
        }
    }

    let seed: u64 = 0xDAFE_1234_5678_9ABC;
    let mut rng = Rng(seed);

    let mut dave = DaveHandler::new(1, 2);
    // Initialize session with session description
    dave.handle(&json_event(
        OP_SESSION_DESCRIPTION,
        json!({ "mode": "aead_aes256_gcm_rtpsize", "dave_protocol_version": 1 }),
    ));

    let binary_ops = [
        OP_MLS_EXTERNAL_SENDER,
        OP_MLS_PROPOSALS,
        OP_MLS_WELCOME,
        OP_MLS_ANNOUNCE_COMMIT_TRANSITION,
    ];

    let seed_payloads = [
        vec![0x00, 0x01, 0xAA, 0xBB],
        vec![0x00, 0x00],
        vec![0x12, 0x34, 0x56, 0x78, 0x9A],
        vec![0xFF, 0xFF, 0x00],
        vec![],
    ];

    let total = 5_000;
    let mut first_step_rejected = 0;
    let mut deep_path_reached = 0;

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    for iteration in 0..total {
        let (event, is_deep) = if rng.next_usize(100) < 75 {
            // Structure-aware mutation: valid headers, optypes, transition IDs, mutated payloads
            let op = binary_ops[rng.next_usize(binary_ops.len())];
            match op {
                OP_MLS_PROPOSALS => {
                    // Valid optype (0=APPEND or 1=REVOKE) + mutated proposal payload
                    let optype = (rng.next_usize(2) as u8) & 0x01;
                    let payload_len = 1 + rng.next_usize(32);
                    let mut body = Vec::with_capacity(payload_len + 1);
                    body.push(optype);
                    for _ in 0..payload_len {
                        body.push((rng.next() & 0xFF) as u8);
                    }
                    (VoiceEvent::Dave { op, body }, true)
                }
                OP_MLS_ANNOUNCE_COMMIT_TRANSITION | OP_MLS_WELCOME => {
                    // Valid u16 transition_id (2 bytes) + mutated MLS commit/welcome payload
                    let tid = (rng.next() & 0xFFFF) as u16;
                    let payload_len = 2 + rng.next_usize(48);
                    let mut body = Vec::with_capacity(payload_len + 2);
                    body.extend_from_slice(&tid.to_be_bytes());
                    for _ in 0..payload_len {
                        body.push((rng.next() & 0xFF) as u8);
                    }
                    (VoiceEvent::Dave { op, body }, true)
                }
                OP_MLS_EXTERNAL_SENDER => {
                    // Non-empty external sender credential
                    let payload_len = 4 + rng.next_usize(32);
                    let mut body = Vec::with_capacity(payload_len);
                    for _ in 0..payload_len {
                        body.push((rng.next() & 0xFF) as u8);
                    }
                    (VoiceEvent::Dave { op, body }, true)
                }
                _ => unreachable!(),
            }
        } else {
            // Raw mutation: flip, truncate, extend, splice across seed payloads
            let op = binary_ops[rng.next_usize(binary_ops.len())];
            let base = &seed_payloads[rng.next_usize(seed_payloads.len())];
            let mut mutated = base.clone();
            match rng.next_usize(4) {
                0 => {
                    let count = 1 + rng.next_usize(4);
                    for _ in 0..count {
                        if !mutated.is_empty() {
                            let idx = rng.next_usize(mutated.len());
                            mutated[idx] ^= (rng.next() & 0xFF) as u8;
                        }
                    }
                }
                1 => {
                    let new_len = rng.next_usize(mutated.len() + 1);
                    mutated.truncate(new_len);
                }
                2 => {
                    let extra = 1 + rng.next_usize(32);
                    for _ in 0..extra {
                        mutated.push((rng.next() & 0xFF) as u8);
                    }
                }
                3 => {
                    if !mutated.is_empty() {
                        let start = rng.next_usize(mutated.len());
                        let count = rng.next_usize(mutated.len() - start);
                        for byte in mutated.iter_mut().skip(start).take(count) {
                            *byte = (rng.next() & 0xFF) as u8;
                        }
                    }
                }
                _ => unreachable!(),
            }

            // Determine if framing reached deep path
            let reached_deep = match op {
                OP_MLS_PROPOSALS => !mutated.is_empty() && (mutated[0] == 0 || mutated[0] == 1),
                OP_MLS_ANNOUNCE_COMMIT_TRANSITION | OP_MLS_WELCOME => mutated.len() >= 2,
                OP_MLS_EXTERNAL_SENDER => !mutated.is_empty(),
                _ => false,
            };
            (VoiceEvent::Dave { op, body: mutated }, reached_deep)
        };

        if is_deep {
            deep_path_reached += 1;
        } else {
            first_step_rejected += 1;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = dave.handle(&event);
        }));
        if result.is_err() {
            panic!(
                "DaveHandler::handle panicked at iteration {iteration} with seed {seed:#x}, event: {event:?}"
            );
        }
    }

    std::panic::set_hook(prev_hook);

    let deep_pct = (deep_path_reached as f64 / total as f64) * 100.0;
    println!(
        "DAVE handler: total={total}, first_step_rejected={first_step_rejected}, deep_path_reached={deep_path_reached} ({deep_pct:.2}%)"
    );
    assert!(deep_pct > 20.0, "Expected > 20% deep path reach, got {deep_pct:.2}%");
}
