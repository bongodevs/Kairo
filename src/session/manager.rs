//! The registry of active sessions and of sessions waiting to be resumed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use rand::Rng;

use crate::session::context::SocketContext;
use crate::utils::locked;

const SESSION_ID_LEN: usize = 16;
const SESSION_ID_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Default cap on parked sessions, overridden by `lavalink.server.resumableSessionLimit`.
const DEFAULT_RESUMABLE_SESSION_LIMIT: usize = 4096;

/// Holds the active sessions and the sessions currently paused awaiting resume.
///
/// Lock order: never hold two locks at once. Every method takes one guard, drops it, then takes the
/// other if it still needs to.
pub struct SocketServer {
    sessions: Mutex<HashMap<String, Arc<SocketContext>>>,
    resumable: Mutex<HashMap<String, Arc<SocketContext>>>,
    // Park order, so the limit evicts the oldest session first.
    resumable_order: Mutex<VecDeque<String>>,
    // Parked sessions already claimed by a handshake that has not attached yet. Kept apart from the
    // resumable map so the session stays reachable while two handshakes race for the same id.
    claimed_resumable: Mutex<HashSet<String>>,
    resumable_limit: usize,
}

impl Default for SocketServer {
    fn default() -> Self {
        Self::new()
    }
}

impl SocketServer {
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_RESUMABLE_SESSION_LIMIT)
    }

    /// Create a registry that keeps at most `resumable_limit` parked sessions.
    pub fn with_limit(resumable_limit: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            resumable: Mutex::new(HashMap::new()),
            resumable_order: Mutex::new(VecDeque::new()),
            claimed_resumable: Mutex::new(HashSet::new()),
            resumable_limit: resumable_limit.max(1),
        }
    }

    /// Generate a fresh session id that no live or resumable session is using.
    pub fn generate_session_id(&self) -> String {
        let mut rng = rand::thread_rng();
        loop {
            let id: String = (0..SESSION_ID_LEN)
                .map(|_| SESSION_ID_CHARS[rng.gen_range(0..SESSION_ID_CHARS.len())] as char)
                .collect();
            // The id is not reserved atomically, so two generators could in principle settle on the
            // same one. Reserving it would mean holding both maps at once.
            if !self.is_attachable(&id) {
                return id;
            }
        }
    }

    pub fn insert(&self, context: Arc<SocketContext>) {
        locked(&self.sessions).insert(context.session_id.clone(), context);
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        locked(&self.sessions).get(session_id).cloned()
    }

    /// Look a session up whether it is live or parked, without moving it between the maps.
    pub fn get_any(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        self.get(session_id)
            .or_else(|| locked(&self.resumable).get(session_id).cloned())
    }

    pub fn remove(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        locked(&self.sessions).remove(session_id)
    }

    /// Move an active session into the resumable set after a resumable disconnect.
    pub fn move_to_resumable(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        let context = self.move_session(&self.sessions, &self.resumable, session_id, || {})?;
        locked(&self.claimed_resumable).remove(session_id);
        let mut order = locked(&self.resumable_order);
        order.retain(|id| id != session_id);
        order.push_back(session_id.to_string());
        drop(order);
        self.enforce_resumable_limit();
        Some(context)
    }

    /// Destroy the oldest parked sessions until the cap is respected.
    fn enforce_resumable_limit(&self) {
        loop {
            let evicted = {
                let mut order = locked(&self.resumable_order);
                if order.len() <= self.resumable_limit {
                    return;
                }
                order.pop_front()
            };
            let Some(id) = evicted else { return };
            let context = locked(&self.resumable).remove(&id);
            if let Some(context) = context {
                tracing::info!(
                    session = %id,
                    limit = self.resumable_limit,
                    "resumable session limit reached; destroying the oldest parked session"
                );
                context.shutdown();
            }
        }
    }

    /// Take a resumable session back into the active set on reconnect.
    pub fn take_resumable(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        let context = self.move_session(&self.resumable, &self.sessions, session_id, || {})?;
        locked(&self.resumable_order).retain(|id| id != session_id);
        locked(&self.claimed_resumable).remove(session_id);
        Some(context)
    }

    /// Claim a parked session for one handshake. The session stays in the resumable map so REST can
    /// still reach it, but a second claim for the same id returns `None`, so exactly one handshake
    /// can resume it.
    pub fn claim_resumable(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        {
            let mut claimed = locked(&self.claimed_resumable);
            if !claimed.insert(session_id.to_string()) {
                return None;
            }
        }
        let context = locked(&self.resumable).get(session_id).cloned();
        if context.is_none() {
            locked(&self.claimed_resumable).remove(session_id);
        }
        context
    }

    /// Release a claim without attaching, leaving the session parked where it was.
    pub fn release_claim(&self, session_id: &str) {
        locked(&self.claimed_resumable).remove(session_id);
    }

    /// Finish a claimed resume: drop the parked entry and its claim. The handshake already holds the
    /// context, so the return value only says whether the parked entry was still there.
    pub fn finish_claim(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        self.release_claim(session_id);
        let context = locked(&self.resumable).remove(session_id);
        if context.is_some() {
            locked(&self.resumable_order).retain(|id| id != session_id);
        }
        context
    }

    /// Move a session between the maps. Insert first, so a concurrent reader never misses it; the
    /// transient duplicate is harmless and ends when the source entry is removed.
    fn move_session(
        &self,
        from: &Mutex<HashMap<String, Arc<SocketContext>>>,
        to: &Mutex<HashMap<String, Arc<SocketContext>>>,
        session_id: &str,
        between: impl FnOnce(),
    ) -> Option<Arc<SocketContext>> {
        let context = locked(from).get(session_id).cloned()?;
        locked(to).insert(session_id.to_string(), Arc::clone(&context));
        between();
        locked(from).remove(session_id);
        Some(context)
    }

    /// Drop a resumable session whose resume window expired.
    pub fn drop_resumable(&self, session_id: &str) -> Option<Arc<SocketContext>> {
        let context = locked(&self.resumable).remove(session_id);
        if context.is_some() {
            locked(&self.resumable_order).retain(|id| id != session_id);
            locked(&self.claimed_resumable).remove(session_id);
        }
        context
    }

    /// Whether a handshake may claim `session_id`, live or resumable.
    pub fn is_attachable(&self, session_id: &str) -> bool {
        let resumable = locked(&self.resumable).contains_key(session_id);
        if resumable {
            return true;
        }
        locked(&self.sessions).contains_key(session_id)
    }

    /// All active session contexts, for the periodic broadcast tasks.
    pub fn active_contexts(&self) -> Vec<Arc<SocketContext>> {
        locked(&self.sessions).values().cloned().collect()
    }

    /// Total number of players across all active sessions.
    pub fn total_players(&self) -> usize {
        locked(&self.sessions)
            .values()
            .map(|c| c.player_count())
            .sum()
    }

    /// Of those, how many hold a track and are not paused.
    pub fn total_playing_players(&self) -> usize {
        locked(&self.sessions)
            .values()
            .map(|c| c.playing_player_count())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Parking must not grow without bound: past the cap the oldest session is destroyed.
    #[test]
    fn the_resumable_map_evicts_the_oldest_session_at_the_limit() {
        let server = SocketServer::with_limit(2);
        for id in ["a", "b", "c"] {
            let (sender, _receiver) = tokio::sync::mpsc::channel(8);
            let context = SocketContext::new(
                id.to_string(),
                1,
                player::AudioPlayerManager::new(),
                None,
                None,
                sender,
            );
            server.insert(context);
        }

        for id in ["a", "b", "c"] {
            assert!(server.move_to_resumable(id).is_some(), "{id} was parked");
        }

        assert!(
            server.get_any("a").is_none(),
            "the oldest parked session must be destroyed"
        );
        assert!(server.get_any("b").is_some());
        assert!(server.get_any("c").is_some());
        assert_eq!(locked(&server.resumable).len(), 2);
    }

    // A parked session must stay reachable from `get_any` for the whole move, including the point
    // between the two map updates, or a REST call made during a reconnect sees a spurious 404.
    #[test]
    fn a_session_stays_reachable_while_it_moves_between_maps() {
        let server = SocketServer::new();
        let (sender, _receiver) = tokio::sync::mpsc::channel(8);
        let context = SocketContext::new(
            "session".to_string(),
            1,
            player::AudioPlayerManager::new(),
            None,
            None,
            sender,
        );
        server.insert(Arc::clone(&context));

        server.move_session(&server.sessions, &server.resumable, "session", || {
            assert!(
                server.get_any("session").is_some(),
                "a session being parked must stay reachable"
            );
        });
        assert!(server.get("session").is_none());
        assert!(server.get_any("session").is_some());

        server.move_session(&server.resumable, &server.sessions, "session", || {
            assert!(
                server.get_any("session").is_some(),
                "a session being resumed must stay reachable"
            );
        });
        assert!(server.get("session").is_some());
    }

    #[test]
    fn session_ids_stay_in_the_alphabet() {
        let server = SocketServer::new();
        for _ in 0..64 {
            let id = server.generate_session_id();
            assert_eq!(id.len(), SESSION_ID_LEN);
            assert!(
                id.bytes().all(|b| SESSION_ID_CHARS.contains(&b)),
                "{id} escapes a-z0-9"
            );
        }
    }

    // Generating ids while another thread probes for attachability must not deadlock: the two paths
    // touch both maps, so they have to agree on never holding them together.
    #[test]
    fn concurrent_generate_and_probe_do_not_deadlock() {
        let server = Arc::new(SocketServer::new());
        let probe = {
            let server = Arc::clone(&server);
            std::thread::spawn(move || {
                for _ in 0..5_000 {
                    server.is_attachable("whoever");
                }
            })
        };
        for _ in 0..5_000 {
            server.generate_session_id();
        }
        probe.join().expect("probe thread panicked");
    }
}
