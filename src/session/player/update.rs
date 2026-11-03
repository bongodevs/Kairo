//! Periodic player update emission and state snapshot formatting.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::task::AbortHandle;
use voice::ConnectionState;

use super::LavalinkPlayer;
use crate::protocol::message::Message;
use crate::protocol::player::{CrossfadeSettings, Player, PlayerState, VoiceState};
use crate::utils::locked;
use crate::utils::time::now_millis;

pub(super) static UPDATE_INTERVAL_SECS: OnceLock<u64> = OnceLock::new();

/// Record the configured `playerUpdateInterval`. Called once from `AppState::new`.
pub fn set_update_interval(secs: u64) {
    let _ = UPDATE_INTERVAL_SECS.set(secs.max(1));
}

impl LavalinkPlayer {
    pub(super) fn player_update_message(&self) -> Message {
        Message::PlayerUpdate {
            guild_id: self.guild_id.to_string(),
            state: self.current_state(),
        }
    }

    /// Send a player update to the owning context, unless the session is parked for resume.
    pub fn send_player_update(&self) {
        let Some(context) = self.shared.context.upgrade() else {
            return;
        };
        // Snapshots taken while parked are stale; the resume handshake sends fresh ones.
        if context.is_paused() {
            return;
        }
        context.send_message(self.player_update_message());
    }

    // Arm this player's periodic update timer, phased to now.
    //
    // One timer per player, rather than a node-wide ticker: a shared ticker phases every guild to
    // process start, so a track beginning just after a tick waits out the rest of the interval
    // before its first periodic update. The first tick here fires immediately.
    pub(super) fn start_update_task(self: &Arc<Self>) {
        let interval = Duration::from_secs(UPDATE_INTERVAL_SECS.get().copied().unwrap_or(5));
        // Weak, so a destroyed player is not kept alive by its own timer.
        let weak = Arc::downgrade(self);
        let task = self.shared.runtime.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                match weak.upgrade() {
                    Some(player) => player.send_player_update(),
                    None => return,
                }
            }
        });
        self.swap_update_task(Some(task.abort_handle()));
    }

    pub(super) fn stop_update_task(&self) {
        self.swap_update_task(None);
    }

    fn swap_update_task(&self, task: Option<AbortHandle>) {
        let previous = std::mem::replace(&mut *locked(&self.update_task), task);
        if let Some(previous) = previous {
            previous.abort();
        }
    }

    // The live state, reading connection status and heartbeat ping from the voice connection.
    pub(super) fn current_state(&self) -> PlayerState {
        let position = self.position();
        let guard = locked(&self.voice);
        let connected = matches!(
            guard.as_ref().map(|c| c.state()),
            Some(ConnectionState::Connected)
        );
        // `ping` is `-1` while disconnected and before the first heartbeat ack.
        let ping = if connected {
            guard
                .as_ref()
                .and_then(|c| c.ping())
                .map(|ping| ping as i64)
                .unwrap_or(-1)
        } else {
            -1
        };
        PlayerState {
            time: now_millis(),
            position,
            connected,
            ping,
        }
    }

    fn voice_state(&self) -> VoiceState {
        locked(&self.voice_state).clone().unwrap_or_default()
    }

    /// A full protocol snapshot of this player, as returned by the REST API.
    pub fn snapshot(&self) -> Player {
        let state = self.current_state();
        let track = self.shared.current_track().map(|mut track| {
            track.info.position = state.position;
            track
        });
        Player {
            guild_id: self.guild_id.to_string(),
            track,
            volume: self.player.volume(),
            paused: self.player.is_paused(),
            state,
            voice: self.voice_state(),
            filters: locked(&self.filters).clone(),
            crossfade: locked(&self.crossfade)
                .as_ref()
                .map(CrossfadeSettings::from_engine),
        }
    }
}
