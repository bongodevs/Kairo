use std::sync::Arc;

use tokio::sync::mpsc;

use player::playback::{PlayerConfig, PlayerError};
use player::source::Sources;
use player::track::codec::TrackCodecs;

use super::{SessionSink, SessionStore};
use crate::players::{FrameWindow, GuildId, PlayerEntry, PlayerRegistry};

#[cfg(not(test))]
const LIFECYCLE_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(test)]
const LIFECYCLE_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(10);

impl SessionStore {
    /// REST-visible sessions only, as in the reference; a parked session is not addressable.
    pub(crate) fn players(&self, id: &str) -> Option<Arc<PlayerRegistry>> {
        self.lock().open.get(id).map(|session| Arc::clone(&session.registry))
    }

    pub(crate) fn user_id(&self, id: &str) -> Option<u64> {
        self.lock().open.get(id).map(|session| session.user_id)
    }

    pub(crate) fn frame_windows(&self, id: &str) -> Option<Vec<FrameWindow>> {
        self.players(id).map(|players| players.frame_windows())
    }

    pub(crate) fn set_outbound(&self, id: &str, outbound: SessionSink) -> bool {
        let mut sessions = self.lock();
        let Some(session) = sessions.open.get_mut(id) else {
            return false;
        };
        session.outbound = Some(outbound);
        true
    }

    pub(crate) fn outbound(&self, id: &str) -> Option<SessionSink> {
        self.lock().open.get(id).and_then(|session| session.outbound.clone())
    }

    pub(crate) async fn send_lifecycle(&self, id: &str, message: String) {
        let Some(outbound) = self.outbound(id) else {
            tracing::debug!(session_id = id, "lifecycle event for a session without a socket");
            return;
        };
        match outbound.messages.send_timeout(message, LIFECYCLE_SEND_TIMEOUT).await {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(
                    session_id = id,
                    %error,
                    "lifecycle event was not delivered; closing the session"
                );
                self.cancel_session(id, "lifecycle backlog");
            }
        }
    }

    pub(crate) fn send_update(&self, id: &str, guild_id: GuildId, message: String) {
        let Some(outbound) = self.outbound(id) else {
            return;
        };
        match outbound.messages.try_send(message) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(message)) => {
                outbound.updates.push(guild_id, message)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(session_id = id, "outbound queue closed; dropped an update");
            }
        }
    }

    pub(crate) fn cancel_session(&self, id: &str, reason: &str) {
        if let Some(outbound) = self.outbound(id) {
            let _ = outbound.cancel.send(Some(reason.to_owned()));
        }
    }

    pub(crate) fn create_player(
        self: &Arc<Self>,
        session_id: &str,
        guild_id: GuildId,
        sources: Sources,
        codecs: Arc<TrackCodecs>,
        config: PlayerConfig,
    ) -> Result<Option<Arc<PlayerEntry>>, PlayerError> {
        let mut sessions = self.lock();
        let Some(session) = sessions.open.get_mut(session_id) else {
            return Ok(None);
        };
        let existed = session.registry.get(guild_id).is_some();
        let player = session.registry.create(guild_id, sources, codecs, config)?;
        if !existed {
            session.players = session.players.saturating_add(1);
            player.start_events(Arc::clone(self), session_id.to_owned());
            tracing::info!(session_id, guild_id = %guild_id, "player created");
        }
        Ok(Some(player))
    }

    pub(crate) fn remove_player(
        &self,
        session_id: &str,
        guild_id: GuildId,
    ) -> Option<Option<Arc<PlayerEntry>>> {
        let mut sessions = self.lock();
        let session = sessions.open.get_mut(session_id)?;
        let player = session.registry.remove(guild_id);
        if let Some(player) = &player {
            session.players = session.players.saturating_sub(1);
            if player.is_playing() {
                session.playing_players = session.playing_players.saturating_sub(1);
            }
            tracing::info!(session_id, guild_id = %guild_id, "player destroyed");
        }
        Some(player)
    }

    pub(crate) fn set_player_playing(&self, session_id: &str, guild_id: GuildId, playing: bool) {
        let mut sessions = self.lock();
        let session = match sessions.open.get_mut(session_id) {
            Some(session) => session,
            None => match sessions.paused.get_mut(session_id) {
                Some(paused) => &mut paused.session,
                None => return,
            },
        };
        let Some(player) = session.registry.get(guild_id) else {
            return;
        };
        let previous = player.set_playing(playing);
        match (previous, playing) {
            (false, true) => {
                session.playing_players = session.playing_players.saturating_add(1);
            }
            (true, false) => {
                session.playing_players = session.playing_players.saturating_sub(1);
            }
            _ => {}
        }
    }
}
