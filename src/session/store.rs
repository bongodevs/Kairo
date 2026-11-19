use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use crate::players::{GuildId, PlayerEntry, PlayerRegistry};
use crate::utils::locked;

#[path = "players.rs"]
mod players;

const DEFAULT_RESUME_TIMEOUT: i64 = 60;

#[derive(Clone)]
pub(crate) struct SessionSink {
    pub(crate) messages: mpsc::Sender<String>,
    pub(crate) updates: Arc<UpdateQueue>,
    pub(crate) cancel: watch::Sender<Option<String>>,
}

#[derive(Default)]
pub(crate) struct UpdateQueue {
    pending: Mutex<HashMap<GuildId, String>>,
    notify: Notify,
}

impl UpdateQueue {
    pub(crate) fn push(&self, guild_id: GuildId, message: String) {
        self.pending.lock().unwrap_or_else(|poison| poison.into_inner()).insert(guild_id, message);
        self.notify.notify_one();
    }

    pub(crate) fn drain(&self) -> Vec<String> {
        let mut pending = self.pending.lock().unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut *pending).into_values().collect()
    }

    pub(crate) async fn notified(&self) {
        self.notify.notified().await;
    }
}

/// The reference keeps the signed value and lets the scheduler clamp it, so a negative timeout
/// expires the moment the session is closed.
fn resume_window(seconds: i64) -> Duration {
    Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

#[derive(Clone)]
struct Session {
    user_id: u64,
    players: u32,
    playing_players: u32,
    registry: Arc<PlayerRegistry>,
    outbound: Option<SessionSink>,
    resuming: bool,
    timeout: i64,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            user_id: 0,
            players: 0,
            playing_players: 0,
            registry: Arc::new(PlayerRegistry::default()),
            outbound: None,
            resuming: false,
            timeout: DEFAULT_RESUME_TIMEOUT,
        }
    }
}

struct Paused {
    session: Session,
    expiry: JoinHandle<()>,
}

#[derive(Default)]
struct Sessions {
    open: HashMap<String, Session>,
    paused: HashMap<String, Paused>,
}

pub(crate) struct SessionStore {
    inner: Mutex<Sessions>,
    /// Closed sessions whose players are still stopping; drained before the process exits.
    cleanup: AtomicUsize,
    idle: Notify,
    default_timeout: i64,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new(DEFAULT_RESUME_TIMEOUT as u64)
    }
}

/// Players drained from one closed session, still owed a stop and a voice close.
///
/// The websocket task awaits [`shutdown`](Self::shutdown) after it has sent the close frame. If
/// that task is cancelled first, dropping the handle moves the same work onto the runtime and
/// keeps it counted until it finishes.
pub(crate) struct ClosedPlayers {
    store: Arc<SessionStore>,
    players: Vec<Arc<PlayerEntry>>,
}

impl ClosedPlayers {
    pub(crate) async fn shutdown(mut self) {
        let players = std::mem::take(&mut self.players);
        if players.is_empty() {
            return;
        }
        shutdown_players(players, &self.store).await;
    }
}

impl Drop for ClosedPlayers {
    fn drop(&mut self) {
        let players = std::mem::take(&mut self.players);
        if players.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.store.finish_cleanup();
            return;
        };
        let store = Arc::clone(&self.store);
        runtime.spawn(async move {
            shutdown_players(players, &store).await;
        });
    }
}

impl SessionStore {
    pub(crate) fn new(session_timeout: u64) -> Self {
        Self {
            inner: Mutex::new(Sessions::default()),
            cleanup: AtomicUsize::new(0),
            idle: Notify::new(),
            default_timeout: i64::try_from(session_timeout).unwrap_or(DEFAULT_RESUME_TIMEOUT),
        }
    }

    #[cfg(test)]
    pub(crate) fn reserve(&self, id: &str) -> bool {
        self.reserve_for(id, 0)
    }

    pub(crate) fn reserve_for(&self, id: &str, user_id: u64) -> bool {
        let mut sessions = self.lock();
        if sessions.open.contains_key(id) {
            return false;
        }
        sessions.open.insert(
            id.to_owned(),
            Session { user_id, timeout: self.default_timeout, ..Session::default() },
        );
        true
    }

    pub(crate) fn close(self: &Arc<Self>, id: &str) -> ClosedPlayers {
        let mut sessions = self.lock();
        let Some(mut session) = sessions.open.remove(id) else {
            return ClosedPlayers { store: Arc::clone(self), players: Vec::new() };
        };
        if sessions.open.is_empty() {
            self.idle.notify_waiters();
        }
        session.outbound = None;
        if !session.resuming {
            let players = session.registry.drain();
            if !players.is_empty() {
                self.cleanup.fetch_add(1, Ordering::AcqRel);
            }
            drop(sessions);
            return ClosedPlayers { store: Arc::clone(self), players };
        }
        let store = Arc::clone(self);
        let lapse_id = id.to_owned();
        let timeout = resume_window(session.timeout);
        tracing::info!(session_id = id, timeout = session.timeout, "session parked for resume");
        let expiry = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            store.lapse(&lapse_id).await;
        });
        sessions.paused.insert(id.to_owned(), Paused { session, expiry });
        ClosedPlayers { store: Arc::clone(self), players: Vec::new() }
    }

    pub(crate) fn begin_resume(self: &Arc<Self>, id: &str) -> bool {
        let mut sessions = self.lock();
        let Some(paused) = sessions.paused.remove(id) else {
            return false;
        };
        paused.expiry.abort();
        sessions.open.insert(id.to_owned(), paused.session);
        tracing::info!(session_id = id, "session resumed");
        true
    }

    pub(crate) async fn wait_until_idle(self: &Arc<Self>) {
        self.drain_paused().await;
        loop {
            let mut notified = std::pin::pin!(self.idle.notified());
            notified.as_mut().enable();
            let is_idle = {
                let sessions = self.lock();
                sessions.open.is_empty()
                    && sessions.paused.is_empty()
                    && self.cleanup.load(Ordering::Acquire) == 0
            };
            if is_idle {
                return;
            }
            notified.await;
        }
    }

    async fn drain_paused(self: &Arc<Self>) {
        let paused = {
            let mut sessions = self.lock();
            std::mem::take(&mut sessions.paused)
        };
        for (_, entry) in paused {
            entry.expiry.abort();
            let players = entry.session.registry.drain();
            if !players.is_empty() {
                self.cleanup.fetch_add(1, Ordering::AcqRel);
                ClosedPlayers { store: Arc::clone(self), players }.shutdown().await;
            }
        }
    }

    async fn lapse(self: &Arc<Self>, id: &str) {
        let players = self
            .lock()
            .paused
            .remove(id)
            .map(|paused| paused.session.registry.drain())
            .unwrap_or_default();
        tracing::info!(session_id = id, players = players.len(), "resumable session expired");
        if players.is_empty() {
            return;
        }
        self.cleanup.fetch_add(1, Ordering::AcqRel);
        ClosedPlayers { store: Arc::clone(self), players }.shutdown().await;
    }

    fn finish_cleanup(&self) {
        if self.cleanup.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.idle.notify_waiters();
        }
    }

    pub(crate) fn update(
        &self,
        id: &str,
        resuming: Option<bool>,
        timeout: Option<i64>,
    ) -> Option<(bool, i64)> {
        let mut sessions = self.lock();
        let session = sessions.open.get_mut(id)?;
        if let Some(resuming) = resuming {
            session.resuming = resuming;
        }
        if let Some(timeout) = timeout {
            session.timeout = timeout;
        }
        Some((session.resuming, session.timeout))
    }

    #[cfg(test)]
    pub(crate) fn is_paused(&self, id: &str) -> bool {
        self.lock().paused.contains_key(id)
    }

    fn lock(&self) -> MutexGuard<'_, Sessions> {
        locked(&self.inner)
    }

    pub(crate) fn node_totals(&self) -> (u32, u32) {
        let sessions = self.lock();
        sessions.open.values().fold((0, 0), |acc, session| {
            (acc.0.saturating_add(session.players), acc.1.saturating_add(session.playing_players))
        })
    }

    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.lock().open.len()
    }

    #[cfg(test)]
    pub(crate) fn paused(&self) -> usize {
        self.lock().paused.len()
    }

    #[cfg(test)]
    fn set_counts(&self, id: &str, players: u32, playing_players: u32) {
        if let Some(session) = self.lock().open.get_mut(id) {
            session.players = players;
            session.playing_players = playing_players;
        }
    }
}

/// Stops every player (closing its voice connection first) and then releases the cleanup slot.
async fn shutdown_players(players: Vec<Arc<PlayerEntry>>, store: &Arc<SessionStore>) {
    let mut tasks = tokio::task::JoinSet::new();
    for player in players {
        tasks.spawn(async move {
            let _ = player.shutdown().await;
        });
    }
    while tasks.join_next().await.is_some() {}
    store.finish_cleanup();
}

#[cfg(test)]
#[path = "../tests/websocket_store.rs"]
mod tests;
