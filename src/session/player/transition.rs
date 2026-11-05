use super::*;
use crate::utils::locked;

impl LavalinkPlayer {
    pub(super) fn clear_scheduled_transition_marker(&self) {
        if let Some(id) = locked(&self.transition_marker).take() {
            self.player.remove_marker(id);
        }
    }

    pub(super) fn clear_manual_transition_marker(&self) {
        if let Some(id) = locked(&self.manual_transition_marker).take() {
            self.player.remove_marker(id);
        }
    }

    pub(super) fn clear_all_transition_markers(&self) {
        self.clear_scheduled_transition_marker();
        self.clear_manual_transition_marker();
    }

    pub(super) fn effective_transition(&self) -> Option<CrossfadeOptions> {
        let mut options = (*locked(&self.crossfade))?;
        let info = self.player.playing_track()?;

        let fade_is_applicable = options.duration_ms > 0
            && !info.is_stream
            && info.has_known_duration()
            && info.length > options.duration_ms;
        if fade_is_applicable {
            return Some(options);
        }
        if options.gapless {
            options.duration_ms = 0;
            return Some(options);
        }
        None
    }

    // Arm the transition marker for the current base, or immediately pre-buffer an unknown-length
    // gapless successor. A pending engine transition must first promote before another is armed.
    pub(super) fn arm_transition(&self) {
        eprintln!(
            "arm_transition: pending={} next_some={}",
            self.player.has_pending_next(),
            locked(&self.next_track).is_some()
        );
        if self.player.has_pending_next() || locked(&self.next_track).is_none() {
            eprintln!("arm_transition: early return");
            return;
        }
        let Some(info) = self.player.playing_track() else {
            return;
        };
        // A broadcast has no end to be gapless into, so preparing a successor would park a whole
        // extra session, decode thread and request loop included, for as long as the broadcast runs.
        if info.is_stream {
            return;
        }
        let Some(options) = self.effective_transition() else {
            eprintln!("arm_transition: no effective transition");
            return;
        };
        eprintln!("arm_transition: options={options:?} info.length={}", info.length);

        if options.is_gapless() && !info.has_known_duration() {
            self.begin_transition(options, false);
            return;
        }

        let timecode = if options.is_gapless() {
            info.length.saturating_sub(GAPLESS_LOOKAHEAD_MS)
        } else {
            info.length.saturating_sub(options.duration_ms)
        };
        let context = self.shared.context.clone();
        let guild_id = self.guild_id;
        let handler: player::track::marker::TrackMarkerHandler = Box::new(move |state| {
            eprintln!("arm_transition marker fired: {state:?}");
            if !matches!(
                state,
                MarkerState::Reached | MarkerState::Late | MarkerState::Bypassed
            ) {
                return;
            }
            if let Some(player) = context
                .upgrade()
                .and_then(|context| context.get_player(guild_id))
            {
                if let Some(options) = player.effective_transition() {
                    player.begin_transition(options, true);
                }
            }
        });
        eprintln!("arm_transition: installing marker at timecode {timecode}");
        let marker = TrackMarker::new(timecode, handler);
        let id = marker.id();
        *locked(&self.transition_marker) = Some(id);
        self.player.add_marker(marker);
    }

    pub(super) fn begin_transition(&self, options: CrossfadeOptions, marker_fired: bool) {
        if marker_fired {
            // The tracker removed this marker before invoking us. Taking only the stored id avoids
            // recursively locking the tracker from inside its own callback.
            locked(&self.transition_marker).take();
        } else {
            self.clear_scheduled_transition_marker();
        }
        let Some((track, proto)) = locked(&self.next_track).take() else {
            return;
        };
        if self.shared.begin_transition(proto.clone()).is_none() {
            *locked(&self.next_track) = Some((track, proto));
            return;
        }
        if !self.player.prepare_next(track, options, 0) {
            self.shared.abort_transition();
        }
    }

    /// Trigger the queued successor for an explicit skip. Gapless transitions promote immediately;
    /// crossfades keep the overlap audible and promote when the fade window expires.
    pub fn transition_now(&self) -> bool {
        if self.player.has_pending_next() {
            return self.player.promote_pending_now();
        }

        let Some(mut options) = *locked(&self.crossfade) else {
            return false;
        };
        if options.duration_ms > 0 {
            options.duration_ms = options.manual_duration_ms;
        } else if !options.gapless {
            return false;
        }
        self.begin_transition(options, false);
        if !self.player.has_pending_next() {
            return false;
        }

        if options.is_gapless() {
            return self.player.promote_pending_now();
        }

        let position = self.position().max(0) as u64;
        let context = self.shared.context.clone();
        let guild_id = self.guild_id;
        let handler: player::track::marker::TrackMarkerHandler = Box::new(move |state| {
            if !matches!(state, MarkerState::Reached | MarkerState::Late) {
                return;
            }
            if let Some(player) = context
                .upgrade()
                .and_then(|context| context.get_player(guild_id))
            {
                locked(&player.manual_transition_marker).take();
                player.player.promote_pending_now();
            }
        });
        let marker = TrackMarker::new(position.saturating_add(options.duration_ms), handler);
        let Some(id) = self.player.add_marker(marker) else {
            self.player.promote_pending_now();
            return true;
        };
        *locked(&self.manual_transition_marker) = Some(id);
        true
    }
}
