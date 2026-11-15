use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use super::registry::PlayerEntry;
use crate::utils::time::now_millis;
const FRAMES_PER_MINUTE: u64 = 60_000 / 20;
const MINUTE_MS: u64 = 60_000;
const CONTINUITY_GAP_MS: u64 = 100;
const NEVER: u64 = u64::MAX;

struct MinuteSlot {
    minute: AtomicU64,
    sent: AtomicU64,
    nulled: AtomicU64,
}

impl MinuteSlot {
    const fn new() -> Self {
        Self { minute: AtomicU64::new(NEVER), sent: AtomicU64::new(0), nulled: AtomicU64::new(0) }
    }

    fn record(&self, minute: u64, sent: bool) {
        if self.minute.load(Ordering::Acquire) != minute {
            self.sent.store(0, Ordering::Relaxed);
            self.nulled.store(0, Ordering::Relaxed);
            self.minute.store(minute, Ordering::Release);
        }
        if sent {
            self.sent.fetch_add(1, Ordering::Relaxed);
        } else {
            self.nulled.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn read(&self, minute: u64) -> (u64, u64) {
        if self.minute.load(Ordering::Acquire) != minute {
            return (0, 0);
        }
        (self.sent.load(Ordering::Relaxed), self.nulled.load(Ordering::Relaxed))
    }
}

pub(crate) struct FrameCounter {
    slots: [MinuteSlot; 2],
    playing_since: AtomicU64,
    last_started: AtomicU64,
    last_ended: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameWindow {
    pub(crate) sent: u64,
    pub(crate) nulled: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct FrameStats {
    sent: u64,
    nulled: u64,
    deficit: i64,
}

pub(crate) fn summarize(windows: &[FrameWindow]) -> Option<FrameStats> {
    let count = u64::try_from(windows.len()).ok().filter(|count| *count != 0)?;
    let sent = windows.iter().map(|window| window.sent).sum::<u64>();
    let nulled = windows.iter().map(|window| window.nulled).sum::<u64>();
    let expected = i128::from(count) * i128::from(FRAMES_PER_MINUTE);
    let deficit = expected - i128::from(sent) - i128::from(nulled);
    Some(FrameStats {
        sent: sent / count,
        nulled: nulled / count,
        deficit: (deficit / i128::from(count)).clamp(i64::MIN.into(), i64::MAX.into()) as i64,
    })
}

impl FrameCounter {
    pub(crate) const fn new() -> Self {
        Self {
            slots: [MinuteSlot::new(), MinuteSlot::new()],
            playing_since: AtomicU64::new(NEVER),
            last_started: AtomicU64::new(NEVER / 2),
            last_ended: AtomicU64::new(NEVER),
        }
    }

    pub(crate) fn record(&self, sent: bool) {
        self.record_at(sent, now_millis());
    }

    pub(crate) fn set_playing(&self, playing: bool) {
        let now = now_millis();
        if playing {
            self.start_at(now);
        } else {
            self.end_at(now);
        }
    }

    pub(crate) fn window(&self) -> Option<FrameWindow> {
        self.window_at(now_millis())
    }

    fn record_at(&self, sent: bool, now_ms: u64) {
        let minute = now_ms / MINUTE_MS;
        self.slots[slot(minute)].record(minute, sent);
    }

    fn start_at(&self, now_ms: u64) {
        self.last_started.store(now_ms, Ordering::Release);
        let ended = self.last_ended.load(Ordering::Acquire);
        let playing_since = self.playing_since.load(Ordering::Acquire);
        if playing_since == NEVER
            || (ended != NEVER && now_ms.saturating_sub(ended) > CONTINUITY_GAP_MS)
        {
            self.playing_since.store(now_ms, Ordering::Release);
            self.last_ended.store(NEVER, Ordering::Release);
        }
    }

    fn end_at(&self, now_ms: u64) {
        self.last_ended.store(now_ms, Ordering::Release);
    }

    fn window_at(&self, now_ms: u64) -> Option<FrameWindow> {
        let current_minute = now_ms / MINUTE_MS;
        let previous_minute = current_minute.checked_sub(1)?;
        let previous_start = previous_minute.saturating_mul(MINUTE_MS);
        if self.playing_since.load(Ordering::Acquire) >= previous_start {
            return None;
        }
        let ended = self.last_ended.load(Ordering::Acquire);
        let started = self.last_started.load(Ordering::Acquire);
        if ended != NEVER && started.saturating_sub(ended) > CONTINUITY_GAP_MS {
            return None;
        }
        let (sent, nulled) = self.slots[slot(previous_minute)].read(previous_minute);
        Some(FrameWindow { sent, nulled })
    }
}

impl PlayerEntry {
    pub(super) fn frame_window(&self) -> Option<FrameWindow> {
        if !self.is_playing() {
            return None;
        }
        self.frame_counter.window()
    }
}

const fn slot(minute: u64) -> usize {
    (minute & 1) as usize
}

#[cfg(test)]
#[path = "../tests/player_frame_stats.rs"]
mod tests;
