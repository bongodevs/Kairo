use crate::gateway::{CloseAction, Intro, close_action};

pub(crate) const CLOSE_NORMAL: u16 = 1000;
pub(crate) const CLOSE_INTERNAL: u16 = 4000;
const CLOSE_SESSION_LOST: u16 = 4006;
pub(crate) const CLOSE_ABNORMAL: u16 = 1006;
pub(crate) const CLOSE_RECONNECT: u16 = 4900;
pub(crate) const RECONNECT_REASON: &str = "Kairo: reconnect";

const MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disconnect {
    pub code: u16,
    pub reason: String,
    pub by_remote: bool,
}

impl Disconnect {
    pub(crate) fn remote(code: u16, reason: impl Into<String>) -> Self {
        Self { code, reason: reason.into(), by_remote: true }
    }

    pub(crate) fn local(code: u16, reason: impl Into<String>) -> Self {
        Self { code, reason: reason.into(), by_remote: false }
    }

    pub(crate) fn abnormal(reason: impl Into<String>) -> Self {
        Self::local(CLOSE_ABNORMAL, reason)
    }

    pub(crate) fn internal(reason: impl Into<String>) -> Self {
        Self::local(CLOSE_INTERNAL, reason)
    }

    pub fn session_lost(&self) -> bool {
        self.code == CLOSE_SESSION_LOST
    }
}

#[derive(Debug, Default)]
pub(crate) struct Retry {
    attempts: u32,
    resumable: bool,
}

impl Retry {
    pub(crate) fn saw_ready(&mut self) {
        self.resumable = true;
    }

    pub(crate) fn described(&mut self) {
        self.attempts = 0;
    }

    pub(crate) fn attempts(&self) -> u32 {
        self.attempts
    }

    pub(crate) fn attempt(&mut self, code: u16, seq_ack: i64) -> Option<Intro> {
        if self.attempts >= MAX_ATTEMPTS {
            return None;
        }
        match close_action(code) {
            CloseAction::Fatal => None,
            CloseAction::Reidentify if self.resumable => {
                self.resumable = false;
                self.attempts += 1;
                Some(Intro::Identify)
            }
            CloseAction::Resume => {
                self.attempts += 1;
                Some(if self.resumable { Intro::Resume(seq_ack) } else { Intro::Identify })
            }
            CloseAction::Reidentify => None,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/disconnect.rs"]
mod tests;
