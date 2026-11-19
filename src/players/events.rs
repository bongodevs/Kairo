use std::sync::{Arc, Weak};

use serde_json::json;

use player::playback::{ExceptionSeverity, PlayerEvent, PlayerEvents, TrackEndReason};
use player::track::TrackSnapshot;
use player::track::codec::TrackCodecs;

use super::messages::player_update_message;
use super::registry::PlayerEntry;
use crate::session::SessionStore;
use crate::tracks::encode_snapshot;
use crate::utils::locked;

/// Reported once when generated silence runs out instead of a track end.
const PLACEHOLDER_MESSAGE: &str = "No source could play this track.";

impl PlayerEntry {
    pub(crate) fn start_events(self: &Arc<Self>, store: Arc<SessionStore>, session_id: String) {
        let events = locked(&self.events).take();
        let Some(events) = events else { return };
        let player = Arc::downgrade(self);
        let codecs = self.codec_registry();
        let guild_id = self.guild_id();
        let task = tokio::spawn(run_events(events, player, store, session_id, guild_id, codecs));
        *locked(&self.event_task) = Some(task);
    }
}

async fn run_events(
    mut events: PlayerEvents,
    player: Weak<PlayerEntry>,
    store: Arc<SessionStore>,
    session_id: String,
    guild_id: super::GuildId,
    codecs: Arc<TrackCodecs>,
) {
    while let Some(event) = events.recv().await {
        log_event(&session_id, guild_id, &event);
        let current = player.upgrade();
        if let Some(current) = &current {
            match &event {
                PlayerEvent::TrackEnd { track, reason } => {
                    if current.is_placeholder(track.reference()) {
                        current.clear_placeholder(track.reference());
                        if matches!(reason, TrackEndReason::Crossfade | TrackEndReason::Gapless) {
                            current.set_next_track(None);
                        }
                        store.set_player_playing(&session_id, guild_id, false);
                        if matches!(reason, TrackEndReason::Finished | TrackEndReason::LoadFailed)
                            && let Some(message) = placeholder_exception(track, guild_id, &codecs)
                        {
                            store.send_lifecycle(&session_id, message).await;
                        }
                        continue;
                    }
                }
                PlayerEvent::TrackException { track, .. }
                | PlayerEvent::TrackStuck { track, .. }
                    if current.is_placeholder(track.reference()) =>
                {
                    continue;
                }
                _ => {}
            }
        }
        let Some(message) = event_message(&event, guild_id, &codecs) else {
            continue;
        };
        match event {
            PlayerEvent::TrackStart { track } => {
                if let Some(current) = &current {
                    current.promote_placeholder(track.reference());
                    if current.is_placeholder(track.reference()) {
                        crate::players::resume::start(current);
                    }
                }
                store.send_lifecycle(&session_id, message).await;
                if let Some(player) = player.upgrade() {
                    let playing = player
                        .player()
                        .state()
                        .await
                        .is_ok_and(|state| state.track.is_some() && !state.paused);
                    store.set_player_playing(&session_id, guild_id, playing);
                    player.notify_voice_frames().await;
                    if let Some(update) = player_update_message(&player).await {
                        store.send_update(&session_id, guild_id, update);
                    }
                }
            }
            PlayerEvent::TrackEnd { reason, .. } => {
                if matches!(reason, TrackEndReason::Crossfade | TrackEndReason::Gapless)
                    && let Some(player) = player.upgrade()
                {
                    player.set_next_track(None);
                }
                store.set_player_playing(&session_id, guild_id, false);
                store.send_lifecycle(&session_id, message).await;
            }
            PlayerEvent::TrackStuck { .. } => {
                store.send_lifecycle(&session_id, message).await;
                if let Some(player) = player.upgrade()
                    && let Some(update) = player_update_message(&player).await
                {
                    store.send_update(&session_id, guild_id, update);
                }
            }
            PlayerEvent::TrackException { .. } => {
                store.send_lifecycle(&session_id, message).await;
            }
            PlayerEvent::Paused { .. } | PlayerEvent::Resumed { .. } => {}
        }
    }
}

fn log_event(session_id: &str, guild_id: super::GuildId, event: &PlayerEvent) {
    match event {
        PlayerEvent::TrackStart { track } => tracing::info!(
            session_id,
            guild_id = %guild_id,
            track = track.reference().identifier(),
            "track started"
        ),
        PlayerEvent::TrackEnd { track, reason } => tracing::info!(
            session_id,
            guild_id = %guild_id,
            track = track.reference().identifier(),
            reason = end_reason(*reason),
            "track ended"
        ),
        PlayerEvent::TrackStuck { track, threshold_ms } => tracing::warn!(
            session_id,
            guild_id = %guild_id,
            track = track.reference().identifier(),
            threshold_ms,
            "track stuck"
        ),
        PlayerEvent::TrackException { track, exception } => tracing::warn!(
            session_id,
            guild_id = %guild_id,
            track = track.reference().identifier(),
            severity = severity(exception.severity),
            message = exception.message.as_deref().unwrap_or(""),
            "track exception"
        ),
        PlayerEvent::Paused { track } | PlayerEvent::Resumed { track } => tracing::debug!(
            session_id,
            guild_id = %guild_id,
            track = track.reference().identifier(),
            "track state changed"
        ),
    }
}

fn event_message(
    event: &PlayerEvent,
    guild_id: super::GuildId,
    codecs: &TrackCodecs,
) -> Option<String> {
    let value = match event {
        PlayerEvent::TrackStart { track } => json!({
            "op": "event",
            "type": "TrackStartEvent",
            "guildId": guild_id.to_string(),
            "track": encode_snapshot(track, codecs).ok()?,
        }),
        PlayerEvent::TrackEnd { track, reason } => json!({
            "op": "event",
            "type": "TrackEndEvent",
            "guildId": guild_id.to_string(),
            "track": encode_snapshot(track, codecs).ok()?,
            "reason": end_reason(*reason),
        }),
        PlayerEvent::TrackException { track, exception } => json!({
            "op": "event",
            "type": "TrackExceptionEvent",
            "guildId": guild_id.to_string(),
            "track": encode_snapshot(track, codecs).ok()?,
            "exception": {
                "message": exception.message,
                "severity": severity(exception.severity),
                "cause": exception.cause,
                "causeStackTrace": exception.cause_stack_trace,
            },
        }),
        PlayerEvent::TrackStuck { track, threshold_ms } => json!({
            "op": "event",
            "type": "TrackStuckEvent",
            "guildId": guild_id.to_string(),
            "track": encode_snapshot(track, codecs).ok()?,
            "thresholdMs": threshold_ms,
        }),
        PlayerEvent::Paused { .. } | PlayerEvent::Resumed { .. } => return None,
    };
    serde_json::to_string(&value).ok()
}

fn placeholder_exception(
    track: &TrackSnapshot,
    guild_id: super::GuildId,
    codecs: &TrackCodecs,
) -> Option<String> {
    event_message(
        &PlayerEvent::TrackException {
            track: track.clone(),
            exception: player::playback::TrackException {
                message: Some(String::from(PLACEHOLDER_MESSAGE)),
                severity: ExceptionSeverity::Common,
                cause: String::from(PLACEHOLDER_MESSAGE),
                cause_stack_trace: String::new(),
            },
        },
        guild_id,
        codecs,
    )
}

fn severity(severity: ExceptionSeverity) -> &'static str {
    match severity {
        ExceptionSeverity::Common => "common",
        ExceptionSeverity::Suspicious => "suspicious",
        ExceptionSeverity::Fault => "fault",
    }
}

fn end_reason(reason: TrackEndReason) -> &'static str {
    match reason {
        TrackEndReason::Finished => "finished",
        TrackEndReason::LoadFailed => "loadFailed",
        TrackEndReason::Stopped => "stopped",
        TrackEndReason::Replaced => "replaced",
        TrackEndReason::Cleanup => "cleanup",
        TrackEndReason::Crossfade => "crossfade",
        TrackEndReason::Gapless => "gapless",
    }
}

#[cfg(test)]
#[path = "../tests/player_events.rs"]
mod tests;
