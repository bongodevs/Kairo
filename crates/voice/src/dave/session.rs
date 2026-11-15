use std::num::NonZeroU16;

use davey::DaveSession;
use serde_json::{Value, json};

use super::fields::{field_u16, field_u64, split_u16, transition_ready};
use super::{
    DaveOut, OP_MLS_COMMIT_WELCOME, OP_MLS_INVALID_COMMIT_WELCOME, OP_MLS_KEY_PACKAGE,
    PASSTHROUGH_EXPIRY,
};

impl super::DaveHandler {
    pub(super) fn prepare_epoch(&mut self, d: &Value) -> Vec<DaveOut> {
        if field_u64(d, "epoch") == 1 {
            self.begin_version(field_u16(d, "protocol_version"))
        } else {
            Vec::new()
        }
    }

    pub(super) fn prepare_transition(&mut self, d: &Value) -> Vec<DaveOut> {
        let transition_id = field_u16(d, "transition_id");
        let version = field_u16(d, "protocol_version");
        if let Some(session) = self.session.as_mut() {
            let _ = session.prepare_transition(transition_id, version);
        }
        if transition_id == 0 {
            // Transition 0 is applied at prepare time; the gateway's later
            // execute is a no-op, exactly as the reference library treats it.
            self.finish_transition(version);
            return Vec::new();
        }
        self.pending_transitions.insert(transition_id, version);
        if version == 0
            && let Some(session) = self.session.as_mut()
        {
            session.set_passthrough_mode(true, None);
        }
        vec![transition_ready(transition_id)]
    }

    pub(super) fn begin_version(&mut self, version: u16) -> Vec<DaveOut> {
        self.protocol_version = version;
        let Some(nonzero) = NonZeroU16::new(version) else {
            return self.go_transport_only();
        };
        // reinit keeps the keys, so readiness alone must not resume encrypted
        // sending: the upgrade transition is what ends transport-only.
        let upgrading = self.downgraded;
        if !self.open_session(nonzero) {
            return Vec::new();
        }
        if upgrading && let Some(session) = self.session.as_mut() {
            session.set_passthrough_mode(true, None);
        }
        self.key_package()
    }

    fn go_transport_only(&mut self) -> Vec<DaveOut> {
        if let Some(session) = self.session.as_mut() {
            let _ = session.reset();
            session.set_passthrough_mode(true, None);
        }
        self.downgraded = true;
        Vec::new()
    }

    fn open_session(&mut self, version: NonZeroU16) -> bool {
        match &mut self.session {
            Some(session) => session.reinit(version, self.user_id, self.channel_id, None).is_ok(),
            None => match DaveSession::new(version, self.user_id, self.channel_id, None) {
                Ok(session) => {
                    self.session = Some(session);
                    true
                }
                Err(err) => {
                    tracing::warn!("DAVE session init failed: {err}");
                    false
                }
            },
        }
    }

    fn key_package(&mut self) -> Vec<DaveOut> {
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        match session.create_key_package() {
            Ok(payload) => vec![DaveOut::Binary { op: OP_MLS_KEY_PACKAGE, payload }],
            Err(err) => {
                tracing::warn!("DAVE key package generation failed: {err}");
                Vec::new()
            }
        }
    }

    pub(super) fn process_proposals(&mut self, body: &[u8]) -> Vec<DaveOut> {
        let Some((raw_optype, proposals)) = body.split_first() else {
            return Vec::new();
        };
        let operation = match *raw_optype {
            0 => davey::ProposalsOperationType::APPEND,
            1 => davey::ProposalsOperationType::REVOKE,
            other => {
                tracing::warn!("unknown DAVE proposals operation type {other}");
                return Vec::new();
            }
        };
        let expected = self.recognized_ids();
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        match session.process_proposals(operation, proposals, Some(&expected)) {
            Ok(Some(cw)) => {
                let mut payload = cw.commit;
                if let Some(welcome) = cw.welcome {
                    payload.extend_from_slice(&welcome);
                }
                vec![DaveOut::Binary { op: OP_MLS_COMMIT_WELCOME, payload }]
            }
            Ok(None) => Vec::new(),
            Err(err) => {
                tracing::warn!("DAVE proposals failed: {err}");
                Vec::new()
            }
        }
    }

    pub(super) fn process_commit(&mut self, body: &[u8]) -> Vec<DaveOut> {
        let Some((transition_id, commit)) = split_u16(body) else {
            return Vec::new();
        };
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        match session.process_commit(commit) {
            Ok(true) => self.after_commit_or_welcome(transition_id),
            Ok(false) => Vec::new(),
            Err(err) => {
                tracing::warn!("DAVE commit errored: {err}");
                self.recover_from_invalid(transition_id)
            }
        }
    }

    pub(super) fn process_welcome(&mut self, body: &[u8]) -> Vec<DaveOut> {
        let Some((transition_id, welcome)) = split_u16(body) else {
            return Vec::new();
        };
        let expected = self.recognized_ids();
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        match session.process_welcome(welcome, Some(&expected)) {
            Ok(()) => self.after_commit_or_welcome(transition_id),
            Err(err) => {
                tracing::warn!("DAVE welcome errored: {err}");
                self.recover_from_invalid(transition_id)
            }
        }
    }

    fn after_commit_or_welcome(&mut self, transition_id: u16) -> Vec<DaveOut> {
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        let version = session.protocol_version().get();
        if session.prepare_transition(transition_id, version).is_err() {
            return self.recover_from_invalid(transition_id);
        }
        if transition_id == 0 {
            self.finish_transition(version);
            return Vec::new();
        }
        self.pending_transitions.insert(transition_id, version);
        vec![transition_ready(transition_id)]
    }

    pub(super) fn execute_pending(&mut self, transition_id: u16) -> Vec<DaveOut> {
        let Some(version) = self.pending_transitions.remove(&transition_id) else {
            // Transition 0 is already applied during prepare; its execute frame
            // is expected and carries no pending work.
            if transition_id != 0 {
                tracing::warn!("execute transition {transition_id} with nothing pending");
            }
            return Vec::new();
        };
        if transition_id != 0
            && let Some(session) = self.session.as_mut()
        {
            match session.execute_transition(transition_id) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!("DAVE execute transition {transition_id} had nothing prepared");
                }
                Err(err) => {
                    tracing::warn!("DAVE execute transition {transition_id} failed: {err}");
                }
            }
        }
        self.finish_transition(version);
        Vec::new()
    }

    fn finish_transition(&mut self, version: u16) {
        let old = std::mem::replace(&mut self.protocol_version, version);
        if old != version && version == 0 {
            self.downgraded = true;
            tracing::debug!("DAVE downgraded to transport-only");
        } else if version > 0 && self.downgraded {
            self.downgraded = false;
            if let Some(session) = self.session.as_mut() {
                // Late unencrypted frames remain valid for ten seconds after
                // an upgrade, then passthrough is refused again.
                session.set_passthrough_mode(false, Some(PASSTHROUGH_EXPIRY));
            }
            tracing::debug!("DAVE upgraded back to E2EE");
        }
    }

    fn recover_from_invalid(&mut self, transition_id: u16) -> Vec<DaveOut> {
        let mut outs = vec![DaveOut::Json {
            op: OP_MLS_INVALID_COMMIT_WELCOME,
            data: json!({ "transition_id": transition_id }),
        }];
        outs.extend(self.begin_version(self.protocol_version));
        outs
    }

    pub(super) fn recognized_ids(&self) -> Vec<u64> {
        self.recognized.iter().copied().collect()
    }
}
