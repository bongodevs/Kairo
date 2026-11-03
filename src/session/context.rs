//! The state behind one WebSocket connection.
//!
//! A context owns the connection's players, its outbound message channel and its resume state.
//! While the socket is live, messages go straight to the write task; while the session is paused,
//! meaning the socket dropped but is resumable, they are queued and replayed on resume.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc::{error::TrySendError, Sender};
use tokio::task::AbortHandle;

use lyrics::LyricsService;
use player::AudioPlayerManager;
use player::CrossfadeOptions;

use crate::protocol::message::Message;
use crate::protocol::stats::FrameStats;
use crate::session::player::{LavalinkPlayer, EXPECTED_PACKET_COUNT_PER_MIN};
use crate::sponsorblock::{MarkableCache, TrackMarkable};

/// Outbound messages buffered for one live socket before it is closed and the client reconnects.
pub(crate) const OUTBOUND_CAPACITY: usize = 1024;

// Lock a session mutex, recovering from poison instead of propagating the panic. One player thread
// panicking while holding a lock must not take the whole session's event delivery down with it.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// Where outbound messages currently go.
enum Outbound {
    // A live socket: the write task drains this channel in order, and a slow client waits here.
    Live(Sender<Message>),
    // Resumable: queue until the client reconnects, oldest first.
    Queued {
        // The pending replay, oldest first.
        queue: VecDeque<Message>,
    },
    // Permanently closed: drop messages.
    Closed,
}

/// Per-connection state for one session.
pub struct SocketContext {
    /// The session id, as used in REST paths and the `ready` message.
    pub session_id: String,
    /// The bot user id from the `User-Id` handshake header.
    pub user_id: u64,
    manager: AudioPlayerManager,
    crossfade_defaults: Option<CrossfadeOptions>,
    // Shared with every player for live-lyrics subscriptions, `None` when lyrics are disabled.
    lyrics_service: Option<Arc<LyricsService>>,
    players: Mutex<HashMap<u64, Arc<LavalinkPlayer>>>,
    outbound: Mutex<Outbound>,
    resuming: AtomicBool,
    resume_timeout_secs: AtomicU64,
    session_paused: AtomicBool,
    // The pending resume-expiry timer. Without cancelling it on resume, a timer armed by an earlier
    // disconnect outlives its resume and expires the next resume window early.
    resume_timeout_task: Mutex<Option<AbortHandle>>,
    // The background stats tick task handle.
    stats_task: Mutex<Option<AbortHandle>>,
    // Bumped whenever a socket takes over this session's outbound channel. A connection whose socket
    // died while the client already reconnected sees a stale epoch and must not park or destroy the
    // session the live socket now owns.
    connection_epoch: AtomicU64,
    sponsorblock: Mutex<HashMap<u64, HashSet<String>>>,
    // SponsorBlock lookups are network-bound and identical for a repeated video, so the session keeps
    // the assembled markers instead of fetching them again.
    sponsorblock_markables: Mutex<MarkableCache>,
    // Outbound messages dropped after the socket or the session is gone. Silent before G13.
    dropped_messages: AtomicU64,
}

impl SocketContext {
    pub fn new(
        session_id: String,
        user_id: u64,
        manager: AudioPlayerManager,
        crossfade_defaults: Option<CrossfadeOptions>,
        lyrics_service: Option<Arc<LyricsService>>,
        sender: Sender<Message>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            user_id,
            manager,
            crossfade_defaults,
            lyrics_service,
            players: Mutex::new(HashMap::new()),
            outbound: Mutex::new(Outbound::Live(sender)),
            resuming: AtomicBool::new(false),
            resume_timeout_secs: AtomicU64::new(60),
            session_paused: AtomicBool::new(false),
            resume_timeout_task: Mutex::new(None),
            stats_task: Mutex::new(None),
            connection_epoch: AtomicU64::new(0),
            sponsorblock: Mutex::new(HashMap::new()),
            sponsorblock_markables: Mutex::new(MarkableCache::default()),
            dropped_messages: AtomicU64::new(0),
        })
    }

    /// The epoch of the socket that currently owns this session's outbound channel.
    pub fn connection_epoch(&self) -> u64 {
        self.connection_epoch.load(Ordering::Acquire)
    }

    /// Send while live, queue while paused, drop once closed.
    pub fn send_message(&self, message: Message) {
        let kind = message.kind();
        let mut outbound = locked(&self.outbound);
        let mut overflowed = false;
        let dropped = match &mut *outbound {
            Outbound::Live(sender) => match sender.try_send(message) {
                Ok(()) => None,
                // A full queue means the client cannot keep up; close it so it reconnects.
                Err(TrySendError::Full(_)) => {
                    overflowed = true;
                    Some("queue full")
                }
                // The write task is gone; the socket is about to close anyway.
                Err(TrySendError::Closed(_)) => Some("socket closed"),
            },
            Outbound::Queued { queue } => {
                queue.push_back(message);
                None
            }
            Outbound::Closed => Some("session closed"),
        };
        if overflowed {
            *outbound = Outbound::Closed;
            tracing::warn!(session = %self.session_id, "outbound queue full; closing the socket");
        }
        if let Some(reason) = dropped {
            self.dropped_messages.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                session = %self.session_id,
                kind,
                reason,
                "dropped an outbound message"
            );
        }
    }

    /// Outbound messages dropped because the socket or the session was already gone.
    pub fn dropped_message_count(&self) -> u64 {
        self.dropped_messages.load(Ordering::Relaxed)
    }

    /// Get the player for `guild_id`, creating it on the first request for that guild.
    pub fn get_or_create_player(self: &Arc<Self>, guild_id: u64) -> Arc<LavalinkPlayer> {
        let mut players = locked(&self.players);
        if let Some(player) = players.get(&guild_id) {
            return Arc::clone(player);
        }
        let engine = self.manager.create_player();
        let player = LavalinkPlayer::new(
            guild_id,
            self.user_id,
            engine,
            self.crossfade_defaults,
            self.lyrics_service.clone(),
            self,
        );
        players.insert(guild_id, Arc::clone(&player));
        player
    }

    pub fn get_player(&self, guild_id: u64) -> Option<Arc<LavalinkPlayer>> {
        locked(&self.players).get(&guild_id).cloned()
    }

    /// Remove and destroy the player for `guild_id`. Returns whether one existed.
    pub fn remove_player(&self, guild_id: u64) -> bool {
        locked(&self.sponsorblock).remove(&guild_id);
        let player = locked(&self.players).remove(&guild_id);
        if let Some(player) = player {
            player.destroy();
            true
        } else {
            false
        }
    }

    pub fn players(&self) -> Vec<Arc<LavalinkPlayer>> {
        locked(&self.players).values().cloned().collect()
    }

    pub fn player_count(&self) -> usize {
        locked(&self.players).len()
    }

    /// The number of players that hold a track and are not paused.
    pub fn playing_player_count(&self) -> usize {
        locked(&self.players)
            .values()
            .filter(|p| p.is_playing())
            .count()
    }

    /// Whether the client asked for this session to survive a dropped socket.
    pub fn is_resuming(&self) -> bool {
        self.resuming.load(Ordering::Acquire)
    }

    /// How long a dropped socket may stay resumable, in seconds.
    pub fn resume_timeout_secs(&self) -> u64 {
        self.resume_timeout_secs.load(Ordering::Acquire)
    }

    pub fn set_resuming(&self, resuming: bool) {
        self.resuming.store(resuming, Ordering::Release);
    }

    pub fn set_resume_timeout_secs(&self, timeout: u64) {
        self.resume_timeout_secs.store(timeout, Ordering::Release);
    }

    /// Whether the socket dropped and the session is waiting to be resumed.
    pub fn is_paused(&self) -> bool {
        self.session_paused.load(Ordering::Acquire)
    }

    /// Pause the session: queue outbound messages until a resume or the resume timeout.
    pub fn pause(&self) {
        self.session_paused.store(true, Ordering::Release);
        let mut outbound = locked(&self.outbound);
        // Keep an existing queue: pausing twice must not discard a pending replay.
        if !matches!(&*outbound, Outbound::Queued { .. }) {
            *outbound = Outbound::Queued {
                queue: VecDeque::new(),
            };
        }
    }

    /// Arm the resume-expiry timer, cancelling any timer left over from an earlier disconnect.
    pub fn arm_resume_timeout(&self, handle: AbortHandle) {
        if let Some(previous) = locked(&self.resume_timeout_task).replace(handle) {
            previous.abort();
        }
    }

    /// Cancel the pending resume-expiry timer.
    pub fn stop_resume_timeout(&self) {
        if let Some(handle) = locked(&self.resume_timeout_task).take() {
            handle.abort();
        }
    }

    /// Store the background stats task handle.
    pub fn set_stats_task(&self, handle: AbortHandle) {
        if let Some(previous) = locked(&self.stats_task).replace(handle) {
            previous.abort();
        }
    }

    /// Cancel the periodic stats task.
    pub fn stop_stats_task(&self) {
        if let Some(handle) = locked(&self.stats_task).take() {
            handle.abort();
        }
    }

    /// Frame stats averaged over this session's playing players, or `None` when none has a usable window.
    pub fn aggregate_frame_stats(&self) -> Option<FrameStats> {
        let mut players: i64 = 0;
        let mut sent: i64 = 0;
        let mut nulled: i64 = 0;
        for player in self.players() {
            if !player.is_playing() || !player.loss_counter().is_data_usable() {
                continue;
            }
            let (success, loss) = player.loss_counter().last_minute();
            players += 1;
            sent += success;
            nulled += loss;
        }

        if players == 0 {
            return None;
        }
        let deficit = players * EXPECTED_PACKET_COUNT_PER_MIN - (sent + nulled);
        Some(FrameStats {
            sent: (sent / players) as i32,
            nulled: (nulled / players) as i32,
            deficit: (deficit / players) as i32,
        })
    }

    /// Resume onto a fresh outbound channel, returning the new epoch and the parked replay.
    pub fn resume_with(&self, sender: Sender<Message>) -> (u64, Vec<Message>) {
        self.stop_resume_timeout();
        let mut outbound = locked(&self.outbound);
        let backlog = match std::mem::replace(&mut *outbound, Outbound::Live(sender)) {
            Outbound::Queued { queue } => queue.into(),
            _ => Vec::new(),
        };
        drop(outbound);
        self.session_paused.store(false, Ordering::Release);
        (
            self.connection_epoch.fetch_add(1, Ordering::AcqRel) + 1,
            backlog,
        )
    }

    /// Replace the outbound channel for a live session, returning the new connection epoch.
    pub fn attach_sender(&self, sender: Sender<Message>) -> u64 {
        self.stop_resume_timeout();
        *locked(&self.outbound) = Outbound::Live(sender);
        self.session_paused.store(false, Ordering::Release);
        self.connection_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// The SponsorBlock categories to skip for a guild.
    pub fn get_sponsorblock_categories(&self, guild_id: u64) -> Option<HashSet<String>> {
        locked(&self.sponsorblock).get(&guild_id).cloned()
    }

    pub fn set_sponsorblock_categories(&self, guild_id: u64, categories: HashSet<String>) {
        locked(&self.sponsorblock).insert(guild_id, categories);
    }

    pub fn remove_sponsorblock_categories(&self, guild_id: u64) {
        locked(&self.sponsorblock).remove(&guild_id);
    }

    /// Markers already fetched for this video and category set in this session.
    pub fn get_sponsorblock_markables(
        &self,
        video_id: &str,
        categories: &HashSet<String>,
    ) -> Option<Arc<Vec<TrackMarkable>>> {
        locked(&self.sponsorblock_markables).get(video_id, categories)
    }

    /// Remember a lookup so a replay of the same video does not hit the network again.
    pub fn cache_sponsorblock_markables(
        &self,
        video_id: &str,
        categories: &HashSet<String>,
        markables: Arc<Vec<TrackMarkable>>,
    ) {
        locked(&self.sponsorblock_markables).insert(video_id, categories, markables);
    }

    /// Permanently shut down: destroy all players and stop accepting messages.
    pub fn shutdown(&self) {
        self.stop_resume_timeout();
        self.stop_stats_task();
        *locked(&self.outbound) = Outbound::Closed;
        let players: Vec<_> = locked(&self.players).drain().map(|(_, p)| p).collect();
        for player in players {
            player.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn context(sender: Sender<Message>) -> Arc<SocketContext> {
        SocketContext::new(
            "session".to_string(),
            1,
            AudioPlayerManager::new(),
            None,
            None,
            sender,
        )
    }

    fn player_update(guild: &str) -> Message {
        Message::PlayerUpdate {
            guild_id: guild.to_string(),
            state: crate::protocol::player::PlayerState {
                time: 1,
                position: 2,
                connected: true,
                ping: 3,
            },
        }
    }

    fn track_event(guild: &str) -> Message {
        Message::event(crate::protocol::message::EmittedEvent::WebSocketClosed {
            guild_id: guild.to_string(),
            code: 4006,
            reason: "test".to_string(),
            by_remote: true,
        })
    }

    #[test]
    fn sponsorblock_markers_are_cached_per_video_and_category_set() {
        let (dead, _dead_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let context = context(dead);
        let sponsor: HashSet<String> = ["sponsor".to_string()].into();
        let music: HashSet<String> = ["music_offtopic".to_string()].into();
        let markables = Arc::new(vec![TrackMarkable::Chapter(
            crate::protocol::sponsorblock::Chapter {
                name: "intro".to_string(),
                start: 0,
                end: 1000,
                duration: 1000,
            },
        )]);

        assert!(context
            .get_sponsorblock_markables("video", &sponsor)
            .is_none());
        context.cache_sponsorblock_markables("video", &sponsor, Arc::clone(&markables));

        assert_eq!(
            context
                .get_sponsorblock_markables("video", &sponsor)
                .expect("cached")
                .len(),
            1
        );
        assert!(context
            .get_sponsorblock_markables("video", &music)
            .is_none());
    }

    #[test]
    fn messages_dropped_after_the_socket_is_gone_are_counted() {
        let (sender, receiver) = mpsc::channel(OUTBOUND_CAPACITY);
        drop(receiver);
        let context = context(sender);

        assert_eq!(context.dropped_message_count(), 0);
        context.send_message(track_event("1"));
        assert_eq!(context.dropped_message_count(), 1);

        // Parking is not a drop: the message is queued for the resume replay.
        context.pause();
        context.send_message(track_event("2"));
        assert_eq!(context.dropped_message_count(), 1);

        context.shutdown();
        context.send_message(track_event("3"));
        assert_eq!(context.dropped_message_count(), 2);
    }

    // A parked session keeps every message, in order, so a long gap replays in full.
    #[test]
    fn resume_queue_keeps_everything_in_order() {
        let (dead, _dead_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let context = context(dead);
        context.pause();
        for n in 0..5_000 {
            // `session_id` is a cheap sequence marker here.
            context.send_message(Message::Ready {
                resumed: false,
                session_id: n.to_string(),
            });
        }

        let (fresh, _fresh_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let (_epoch, backlog) = context.resume_with(fresh);

        let replayed: Vec<_> = backlog
            .into_iter()
            .map(|message| match message {
                Message::Ready { session_id, .. } => session_id,
                other => panic!("unexpected replay: {other:?}"),
            })
            .collect();
        assert_eq!(replayed.len(), 5_000);
        assert_eq!(replayed[0], "0");
        assert_eq!(replayed[4_999], "4999");
    }

    // Everything queues while parked and replays oldest first, events and snapshots alike.
    #[test]
    fn paused_session_queues_and_replays_in_order() {
        let (dead, _dead_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let context = context(dead);
        context.pause();
        context.send_message(player_update("1"));
        context.send_message(track_event("2"));
        context.send_message(player_update("3"));

        let (fresh, _fresh_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let (_epoch, backlog) = context.resume_with(fresh);

        let kinds: Vec<_> = backlog
            .iter()
            .map(|message| match message {
                Message::PlayerUpdate { guild_id, .. } => format!("update:{guild_id}"),
                Message::Event(_) => "event".to_string(),
                other => panic!("unexpected replay: {other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["update:1", "event", "update:3"]);
    }

    // A full live queue closes the socket instead of growing without bound; the session stays live.
    #[test]
    fn a_full_queue_closes_the_socket_without_parking() {
        let (live, mut live_rx) = mpsc::channel(4);
        let context = context(live);

        for n in 0..4 {
            context.send_message(player_update(&n.to_string()));
        }
        // This one finds the queue full and closes the live socket.
        context.send_message(player_update("overflow"));
        assert!(
            !context.is_paused(),
            "a backed-up socket must not park the session"
        );

        let mut updates = 0;
        while let Ok(message) = live_rx.try_recv() {
            match message {
                Message::PlayerUpdate { .. } => updates += 1,
                other => panic!("unexpected message: {other:?}"),
            }
        }
        assert_eq!(updates, 4, "the four queued messages must survive");
        assert!(
            matches!(
                live_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
            ),
            "overflow must close the channel so the pump reconnects the client"
        );
    }
}
