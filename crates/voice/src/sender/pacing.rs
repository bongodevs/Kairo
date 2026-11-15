use std::time::Duration;

use tokio::time::Instant;

use crate::crypto::{MAX_DATAGRAM_BYTES, MAX_PACKET_BYTES};
use crate::udp::rtp::Rtp;
pub(crate) const FRAME_INTERVAL: Duration = Duration::from_millis(20);
pub(crate) const SILENCE_FRAME: [u8; 3] = [0xF8, 0xFF, 0xFE];
pub(crate) const SILENCE_TAIL: usize = 5;
const MAX_FRAMES_BEHIND: u64 = 3;

pub trait FrameSource {
    fn provide(&mut self, out: &mut [u8]) -> Option<usize>;

    fn is_active(&self) -> bool {
        false
    }
}

pub(crate) struct NoFrames;

impl FrameSource for NoFrames {
    fn provide(&mut self, _out: &mut [u8]) -> Option<usize> {
        None
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub sent: u64,
    pub skipped: u64,
}

#[derive(Debug)]
pub(crate) struct Pacing {
    next: Option<Instant>,
    tail: usize,
    sent: u64,
    skipped: u64,
}

impl Pacing {
    fn new() -> Self {
        Self { next: None, tail: 0, sent: 0, skipped: 0 }
    }

    pub(crate) fn arm(&mut self) {
        self.next.get_or_insert_with(Instant::now);
    }

    pub(crate) fn stop(&mut self) {
        self.next = None;
        self.tail = 0;
    }

    pub(crate) fn resume(&mut self) {
        if self.next.is_some() {
            self.next = Some(Instant::now());
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next
    }

    pub(crate) fn advance(&mut self) {
        let Some(due) = self.next else { return };
        let due = due + FRAME_INTERVAL;
        let now = Instant::now();
        let behind =
            (now.saturating_duration_since(due).as_millis() / FRAME_INTERVAL.as_millis()) as u64;
        if behind > MAX_FRAMES_BEHIND {
            self.skipped += behind;
            self.next = Some(now + FRAME_INTERVAL);
        } else {
            self.next = Some(due);
        }
    }

    pub(crate) fn produced(&mut self) {
        self.tail = SILENCE_TAIL;
    }

    pub(crate) fn take_tail(&mut self) -> bool {
        if self.tail == 0 {
            return false;
        }
        self.tail -= 1;
        true
    }

    pub(crate) fn written(&mut self) {
        self.sent += 1;
    }

    pub(crate) fn skip(&mut self) {
        self.skipped += 1;
    }

    pub(crate) fn stats(&self) -> FrameStats {
        FrameStats { sent: self.sent, skipped: self.skipped }
    }
}

pub(crate) struct Media {
    pub(crate) frame: [u8; MAX_PACKET_BYTES],
    pub(crate) encrypted: Vec<u8>,
    pub(crate) sealed: Vec<u8>,
    pub(crate) rtp: Rtp,
    pub(crate) pacing: Pacing,
}

impl Media {
    pub(crate) fn new(ssrc: u32) -> Self {
        Self {
            frame: [0u8; MAX_PACKET_BYTES],
            encrypted: Vec::with_capacity(MAX_DATAGRAM_BYTES),
            sealed: Vec::with_capacity(MAX_DATAGRAM_BYTES),
            rtp: Rtp::new(ssrc),
            pacing: Pacing::new(),
        }
    }

    pub(crate) fn stats(&self) -> FrameStats {
        self.pacing.stats()
    }

    pub(crate) fn rebind(&mut self, ssrc: u32) {
        self.rtp.rebind(ssrc);
        self.pacing.stop();
    }

    pub(crate) fn resume(&mut self) {
        self.pacing.resume();
    }
}

#[cfg(test)]
#[path = "../../tests/media.rs"]
mod tests;
