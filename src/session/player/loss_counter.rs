//! Per-player audio-loss accounting.
//!
//! Each 20 ms frame the player provides is recorded as a success, each frame it had nothing ready
//! for as a loss. Counts accumulate per minute and rotate into a last-full-minute bucket, which is
//! what the node's `frameStats` are derived from.
//!
//! [`is_data_usable`](AudioLossCounter::is_data_usable) gates whether a player contributes at all:
//! one that only just started or resumed has no full minute of data yet, and a real gap in playback
//! makes the bucket meaningless.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::utils::locked;

/// Expected number of 20 ms frames per minute.
pub const EXPECTED_PACKET_COUNT_PER_MIN: i64 = 60 * 1000 / 20;

// How long a stop and the next start may be apart while still counting as a seamless switch.
const ACCEPTABLE_TRACK_SWITCH_TIME: Duration = Duration::from_millis(100);

/// Counts sent against nulled frames over a one-minute sliding window.
pub struct AudioLossCounter {
    start: Instant,
    inner: Mutex<Inner>,
}

// Every instant here is an elapsed duration since `AudioLossCounter::start`.
#[derive(Default)]
struct Inner {
    current_minute: u64,
    cur_success: i64,
    cur_loss: i64,
    last_success: i64,
    last_loss: i64,
    // When the current continuous stretch of playback began.
    playing_since: Option<Duration>,
    last_track_started: Option<Duration>,
    last_track_ended: Option<Duration>,
}

impl AudioLossCounter {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn rotate(&self, inner: &mut Inner) {
        inner.rotate_to(self.start.elapsed().as_secs() / 60);
    }

    /// Record a frame that went out.
    pub fn record_success(&self) {
        let mut inner = locked(&self.inner);
        self.rotate(&mut inner);
        inner.cur_success += 1;
    }

    /// Record a frame the player had no audio ready for.
    pub fn record_loss(&self) {
        let mut inner = locked(&self.inner);
        self.rotate(&mut inner);
        inner.cur_loss += 1;
    }

    /// The last full minute's `(sent, nulled)` counts, which callers aggregate themselves.
    pub fn last_minute(&self) -> (i64, i64) {
        let mut inner = locked(&self.inner);
        self.rotate(&mut inner);
        (inner.last_success, inner.last_loss)
    }

    /// Playback started or resumed.
    pub fn on_playback_started(&self) {
        let now = self.start.elapsed();
        let mut inner = locked(&self.inner);
        inner.last_track_started = Some(now);
        // A seamless switch keeps the window, a real gap or a first start reopens it.
        let gap_exceeded = matches!(
            inner.last_track_ended,
            Some(ended) if now.saturating_sub(ended) > ACCEPTABLE_TRACK_SWITCH_TIME
        );
        if gap_exceeded || inner.playing_since.is_none() {
            inner.playing_since = Some(now);
            inner.last_track_ended = None;
        }
    }

    /// Playback stopped or paused.
    pub fn on_playback_stopped(&self) {
        let now = self.start.elapsed();
        locked(&self.inner).last_track_ended = Some(now);
    }

    /// Whether this player's last-minute bucket is worth reporting.
    pub fn is_data_usable(&self) -> bool {
        locked(&self.inner).data_usable_at(self.start.elapsed())
    }
}

impl Inner {
    // Roll current-minute loss counters into the last-minute bucket on minute transitions.
    fn rotate_to(&mut self, minute: u64) {
        if minute == self.current_minute {
            return;
        }
        let stale = minute.saturating_sub(self.current_minute) > 1;
        self.last_success = if stale { 0 } else { self.cur_success };
        self.last_loss = if stale { 0 } else { self.cur_loss };
        self.cur_success = 0;
        self.cur_loss = 0;
        self.current_minute = minute;
    }

    // The gate as a pure function of the window state at `now`.
    fn data_usable_at(&self, now: Duration) -> bool {
        if let (Some(started), Some(ended)) = (self.last_track_started, self.last_track_ended) {
            if started.saturating_sub(ended) > ACCEPTABLE_TRACK_SWITCH_TIME {
                return false;
            }
        }

        // Playback has to predate the start of the last complete minute, which is the bucket
        // `last_success` and `last_loss` hold.
        let Some(playing_since) = self.playing_since else {
            return false;
        };
        let Some(last_minute) = (now.as_secs() / 60).checked_sub(1) else {
            return false;
        };
        playing_since.as_secs() < last_minute * 60
    }
}

impl Default for AudioLossCounter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn never_played_is_never_usable() {
        assert!(!Inner::default().data_usable_at(secs(180)));
    }

    #[test]
    fn needs_a_full_minute_of_playback() {
        let playing_long_enough = Inner {
            playing_since: Some(secs(0)),
            ..Default::default()
        };
        assert!(playing_long_enough.data_usable_at(secs(180)));

        // Started inside the last, incomplete minute: the 120s bucket boundary is the cutoff.
        let just_started = Inner {
            playing_since: Some(secs(130)),
            ..Default::default()
        };
        assert!(!just_started.data_usable_at(secs(180)));
    }

    #[test]
    fn a_real_playback_gap_invalidates_the_window() {
        let gap = Inner {
            playing_since: Some(secs(0)),
            last_track_ended: Some(secs(140)),
            last_track_started: Some(secs(150)),
            ..Default::default()
        };
        assert!(!gap.data_usable_at(secs(180)));

        // A switch inside ACCEPTABLE_TRACK_SWITCH_TIME stays usable.
        let seamless = Inner {
            playing_since: Some(secs(0)),
            last_track_ended: Some(secs(140)),
            last_track_started: Some(secs(140) + Duration::from_millis(50)),
            ..Default::default()
        };
        assert!(seamless.data_usable_at(secs(180)));
    }

    #[test]
    fn pause_then_resume_restarts_the_window() {
        let counter = AudioLossCounter::new();
        counter.on_playback_started();
        counter.on_playback_stopped();
        std::thread::sleep(Duration::from_millis(150));
        counter.on_playback_started();

        let inner = locked(&counter.inner);
        assert!(
            inner.playing_since.unwrap() >= Duration::from_millis(150),
            "a >100ms gap must move playing_since to the resume"
        );
        assert!(inner.last_track_ended.is_none(), "window reopened");
    }

    #[test]
    fn counts_rotate_into_the_last_minute_bucket() {
        let mut inner = Inner {
            cur_success: 1,
            cur_loss: 1,
            ..Default::default()
        };
        // Same minute: nothing rotates.
        inner.rotate_to(0);
        assert_eq!((inner.last_success, inner.last_loss), (0, 0));

        inner.rotate_to(1);
        assert_eq!((inner.last_success, inner.last_loss), (1, 1));
        assert_eq!((inner.cur_success, inner.cur_loss), (0, 0));
    }

    #[test]
    fn an_idle_minute_reports_empty_rather_than_stale_counts() {
        let mut inner = Inner {
            cur_success: 500,
            cur_loss: 7,
            ..Default::default()
        };
        // Nothing recorded for two minutes, so the last complete minute is empty, not minute 0's.
        inner.rotate_to(2);
        assert_eq!((inner.last_success, inner.last_loss), (0, 0));
    }
}
