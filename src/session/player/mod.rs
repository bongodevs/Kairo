//! One guild's player.
//!
//! Wraps the engine's [`player::AudioPlayer`], an optional [`voice::VoiceConnection`] for the
//! Discord send loop, and an [`AudioLossCounter`]. Engine [`AudioEvent`]s are forwarded to the
//! owning [`SocketContext`] as WebSocket events, and the PATCH operations the REST layer applies
//! (play, stop, seek, volume, pause, filters, endTime, voice) all land here.
//!
//! Engine events carry only [`player::AudioTrackInfo`], never the encodable track, so the protocol
//! [`Track`] for the current track and for the one it replaced is kept in [`PlayerShared`]. A
//! `TrackEnd(Replaced)` resolves to the previous track and every other event to the current one.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::{Map, Value};
use tokio::task::AbortHandle;

use player::player::event::{AudioEvent, AudioEventListener};
use player::track::marker::{MarkerId, MarkerState, TrackMarker};
use player::track::state::AudioTrackEndReason as EngineEndReason;
use player::{AudioPlayer, AudioTrack, CrossfadeOptions, TapeOptions};

use ::voice::{ConnectionState, VoiceConnection};

use crate::protocol::filters::Filters;
use crate::protocol::load_result::Exception;
use crate::protocol::message::{EmittedEvent, Message, TrackEndReason};
use crate::protocol::player::VoiceState;
use crate::protocol::track::Track;
use crate::session::context::SocketContext;
use crate::session::lyrics::{query_from_info, PlayerLyrics};
use crate::sponsorblock::{self, TrackMarkable};
use crate::utils::locked;

pub use loss_counter::{AudioLossCounter, EXPECTED_PACKET_COUNT_PER_MIN};
pub use update::set_update_interval;
pub use voice::VoiceConnectError;

use lyrics::LyricsService;

const GAPLESS_LOOKAHEAD_MS: u64 = 500;

// The current track and the one it replaced, as protocol tracks.
#[derive(Default)]
struct TrackSlots {
    current: Option<Track>,
    previous: Option<Track>,
}

/// State shared between a [`LavalinkPlayer`] and the engine event listener it registers.
///
/// The owning [`SocketContext`] is held weakly, since it owns the player in turn.
pub struct PlayerShared {
    guild_id: u64,
    pub(super) context: Weak<SocketContext>,
    pub(super) player: Weak<AudioPlayer>,
    tracks: Mutex<TrackSlots>,
    // Set just before an `endTime` marker stops the track, so the engine `Stopped` end that follows
    // is reported to clients as `Finished`.
    end_marker_hit: AtomicBool,
    transition_outgoing: Mutex<Option<Track>>,
    transition_incoming: Mutex<Option<Track>>,
    // Present only when the lyrics feature is enabled.
    lyrics: Mutex<Option<Arc<PlayerLyrics>>>,
    // True while a crossfade or gapless successor is in flight. Lyrics need it to tell the two
    // starts apart: a normal play seeds on `TrackStart`, but a transition successor has to wait for
    // `TrackPromoted`, because at the overlap-start `TrackStart` it is still a pending mixer layer
    // on the wrong executor.
    in_transition: AtomicBool,
    // The same counter the player holds; one listener drives both.
    loss: Arc<AudioLossCounter>,
    // Captured at construction, so events arriving on the decode thread can hand blocking work to
    // the runtime's bounded pool instead of spawning threads of their own.
    pub(super) runtime: tokio::runtime::Handle,
}

impl PlayerShared {
    fn current_track(&self) -> Option<Track> {
        locked(&self.tracks).current.clone()
    }

    fn lyrics(&self) -> Option<Arc<PlayerLyrics>> {
        locked(&self.lyrics).clone()
    }

    fn previous_track(&self) -> Option<Track> {
        locked(&self.tracks).previous.clone()
    }

    // Make `track` current, moving the old current into `previous` for the `Replaced` event.
    fn replace_current(&self, track: Track) {
        let mut slots = locked(&self.tracks);
        slots.previous = slots.current.take();
        slots.current = Some(track);
    }

    // Move the outgoing track aside and install the successor before the engine emits its
    // transition events, so `TrackStart` for the successor is already resolvable even though the
    // overlap begins before the old executor reaches its terminator.
    fn begin_transition(&self, track: Track) -> Option<Track> {
        let outgoing = self.current_track()?;
        *locked(&self.transition_outgoing) = Some(outgoing.clone());
        *locked(&self.transition_incoming) = Some(track);
        self.in_transition.store(true, Ordering::Release);
        Some(outgoing)
    }

    fn abort_transition(&self) {
        locked(&self.transition_outgoing).take();
        locked(&self.transition_incoming).take();
        self.in_transition.store(false, Ordering::Release);
    }

    fn clear_current(&self) {
        locked(&self.tracks).current = None;
    }

    fn set_user_data(&self, user_data: Map<String, Value>) {
        if let Some(track) = locked(&self.tracks).current.as_mut() {
            track.user_data = user_data;
        }
    }

    // Update the loss counter playback window across pause, resume, start, and end events.
    fn track_loss_window(&self, event: &AudioEvent) {
        match event {
            AudioEvent::TrackStart(_) if self.in_transition.load(Ordering::Acquire) => {}
            AudioEvent::TrackEnd(_, EngineEndReason::Crossfade | EngineEndReason::Gapless) => {}
            AudioEvent::TrackStart(_) | AudioEvent::PlayerResume => self.loss.on_playback_started(),
            AudioEvent::TrackEnd(..) | AudioEvent::PlayerPause => self.loss.on_playback_stopped(),
            _ => {}
        }
    }
}

/// A guild's player: the engine player, the voice connection, and protocol state.
pub struct LavalinkPlayer {
    pub(super) guild_id: u64,
    pub(super) user_id: u64,
    pub(super) player: Arc<AudioPlayer>,
    pub(super) shared: Arc<PlayerShared>,
    pub(super) loss: Arc<AudioLossCounter>,
    pub(super) voice: Mutex<Option<VoiceConnection>>,
    pub(super) voice_state: Mutex<Option<VoiceState>>,
    // 4006 re-identify attempts since the last successful handshake.
    voice_reidentify_attempts: AtomicU32,
    // Serializes a whole `PATCH .../players/{guildId}` body.
    patch_lock: tokio::sync::Mutex<()>,
    pub(super) filters: Mutex<Filters>,
    pub(super) crossfade_defaults: Option<CrossfadeOptions>,
    pub(super) crossfade: Mutex<Option<CrossfadeOptions>>,
    next_track: Mutex<Option<(Box<dyn AudioTrack>, Track)>>,
    transition_marker: Mutex<Option<MarkerId>>,
    pub(super) manual_transition_marker: Mutex<Option<MarkerId>>,
    end_marker: Mutex<Option<MarkerId>>,
    // Present only when the lyrics feature is enabled.
    lyrics: Option<Arc<PlayerLyrics>>,
    // The periodic player-update timer, armed on a track start and cancelled on a track end.
    pub(super) update_task: Mutex<Option<AbortHandle>>,
}

/// The reference's `TrackEndMarkerHandler` stops only on `REACHED`/`BYPASSED`. `Late` means the
/// marker was installed behind the play position, which must not stop the track.
fn end_marker_stops(state: MarkerState) -> bool {
    matches!(state, MarkerState::Reached | MarkerState::Bypassed)
}

impl LavalinkPlayer {
    /// Create a player for `guild_id` owned by `context`.
    pub fn new(
        guild_id: u64,
        user_id: u64,
        player: AudioPlayer,
        crossfade_defaults: Option<CrossfadeOptions>,
        lyrics_service: Option<Arc<LyricsService>>,
        context: &Arc<SocketContext>,
    ) -> Arc<Self> {
        let player = Arc::new(player);
        let loss = Arc::new(AudioLossCounter::new());
        let shared = Arc::new(PlayerShared {
            guild_id,
            context: Arc::downgrade(context),
            player: Arc::downgrade(&player),
            tracks: Mutex::new(TrackSlots::default()),
            end_marker_hit: AtomicBool::new(false),
            transition_outgoing: Mutex::new(None),
            transition_incoming: Mutex::new(None),
            lyrics: Mutex::new(None),
            in_transition: AtomicBool::new(false),
            loss: Arc::clone(&loss),
            runtime: tokio::runtime::Handle::current(),
        });
        player.add_listener(shared.clone() as Arc<dyn AudioEventListener>);

        // Build the live-lyrics handle and its coordinator task only when the feature is on. It
        // needs the runtime, which holds because players are only created from a request handler.
        let lyrics = lyrics_service.map(|service| {
            Arc::new(PlayerLyrics::new(
                service,
                Arc::downgrade(&player),
                Arc::downgrade(context),
                guild_id,
            ))
        });
        if let Some(lyrics) = &lyrics {
            *locked(&shared.lyrics) = Some(Arc::clone(lyrics));
        }

        Arc::new(Self {
            guild_id,
            user_id,
            player,
            shared,
            loss,
            voice: Mutex::new(None),
            voice_state: Mutex::new(None),
            voice_reidentify_attempts: AtomicU32::new(0),
            patch_lock: tokio::sync::Mutex::new(()),
            filters: Mutex::new(Filters::default()),
            crossfade_defaults,
            crossfade: Mutex::new(crossfade_defaults),
            next_track: Mutex::new(None),
            transition_marker: Mutex::new(None),
            manual_transition_marker: Mutex::new(None),
            end_marker: Mutex::new(None),
            lyrics,
            update_task: Mutex::new(None),
        })
    }

    pub fn guild_id(&self) -> u64 {
        self.guild_id
    }

    /// This player's share of the node's frame stats.
    pub fn loss_counter(&self) -> &Arc<AudioLossCounter> {
        &self.loss
    }

    pub fn has_track(&self) -> bool {
        self.shared.current_track().is_some()
    }

    pub fn current_track(&self) -> Option<Track> {
        self.shared.current_track()
    }

    /// Whether the player holds a track and is not paused.
    pub fn is_playing(&self) -> bool {
        self.has_track() && !self.player.is_paused()
    }

    pub fn is_connected(&self) -> bool {
        matches!(
            locked(&self.voice).as_ref().map(|c| c.state()),
            Some(ConnectionState::Connected)
        )
    }

    /// End the current track with the `cleanup` reason if nothing has pulled a frame from it within
    /// `threshold_ms`. A no-op unless a track is loaded.
    ///
    /// The engine records a request on every frame handed out, so this only fires when no voice send
    /// loop is draining the player, for instance after its connection died without a close event the
    /// client acted on. Without it the track stays active forever: a decode thread and an HTTP
    /// connection leak, no track end is emitted, and the client's queue never advances.
    pub fn check_cleanup(&self, threshold_ms: u64) {
        self.player.check_cleanup(threshold_ms);
    }

    pub(super) fn position(&self) -> i64 {
        self.player.position().unwrap_or(0) as i64
    }

    /// Start playing `track`, already encoded as `proto`, replacing any current track.
    pub fn play(&self, track: Box<dyn AudioTrack>, proto: Track) {
        self.play_at(track, proto, 0);
    }

    /// Start playing `track` at `position_ms`, replacing any current track.
    ///
    /// Decoding begins at `position_ms`, so no audio from the start of the track leaks out before
    /// the seek takes effect.
    pub fn play_at(&self, track: Box<dyn AudioTrack>, proto: Track, position_ms: u64) {
        self.clear_all_transition_markers();
        self.shared.abort_transition();
        // Set the protocol slots first, since `play_track_at` fires Replaced and Start synchronously.
        self.shared.replace_current(proto);
        self.player.play_track_at(track, position_ms);
        self.arm_transition();
        // Clients expect an update the moment a track starts, then one per interval.
        self.send_player_update();
    }

    /// Replace or clear the successor supplied by the queue-owning client.
    pub fn set_next_track(&self, next: Option<(Box<dyn AudioTrack>, Track)>) {
        let transition_pending = self.player.has_pending_next();
        self.clear_scheduled_transition_marker();
        if !transition_pending {
            self.clear_manual_transition_marker();
        }
        *locked(&self.next_track) = next;
        self.arm_transition();
    }

    /// Apply a complete per-player override. `None` explicitly disables both transition modes.
    pub fn set_crossfade(&self, options: Option<CrossfadeOptions>) {
        self.clear_all_transition_markers();
        *locked(&self.crossfade) = options;
        self.arm_transition();
    }

    /// Clear the per-player override and restore the server defaults.
    pub fn reset_crossfade(&self) {
        self.clear_all_transition_markers();
        *locked(&self.crossfade) = self.crossfade_defaults;
        self.arm_transition();
    }

    /// Set the client-driven tape effect. `None`, the default, makes pause and resume instant.
    pub fn set_tape(&self, options: Option<TapeOptions>) {
        self.player.set_tape(options);
    }

    pub fn stop(&self) {
        self.clear_all_transition_markers();
        locked(&self.next_track).take();
        self.shared.abort_transition();
        self.player.stop_track();
    }

    /// Seek to `position_ms` and emit a player update.
    pub fn seek(&self, position_ms: i64) {
        let position = position_ms.max(0) as u64;
        self.player.set_position(position);
        // The engine ignores a seek on a non-seekable track, so re-anchoring lyrics would emit a
        // line the listener never reached.
        if self.player.is_seekable() {
            if let Some(lyrics) = &self.lyrics {
                lyrics.on_seek(position);
            }
        }
        self.send_player_update();
    }

    /// Set the volume (`0..=1000`).
    pub fn set_volume(&self, volume: i32) {
        self.player.set_volume(volume);
    }

    /// Pause or resume playback.
    pub fn set_paused(&self, paused: bool) {
        self.player.set_paused(paused);
    }

    /// Replace the filter chain and emit a player update.
    pub fn set_filters(&self, filters: Filters) {
        self.player.options().set_filters(filters.to_engine());
        *locked(&self.filters) = filters;
        self.send_player_update();
    }

    /// Attach user data to the current track.
    pub fn set_user_data(&self, user_data: Map<String, Value>) {
        self.shared.set_user_data(user_data);
    }

    /// Set the millisecond position at which to stop the track. A `PATCH` with `endTime: null`
    /// is a no-op: the reference never clears an installed marker that way.
    pub fn set_end_time(&self, end_time: Option<i64>) {
        let Some(ms) = end_time.filter(|ms| *ms > 0) else {
            return;
        };
        if let Some(id) = locked(&self.end_marker).take() {
            self.player.remove_marker(id);
        }
        let player = Arc::downgrade(&self.player);
        let shared = Arc::downgrade(&self.shared);
        let handler: player::track::marker::TrackMarkerHandler =
            Box::new(move |state: MarkerState| {
                if end_marker_stops(state) {
                    if let (Some(player), Some(shared)) = (player.upgrade(), shared.upgrade()) {
                        shared.end_marker_hit.store(true, Ordering::Release);
                        player.stop_track();
                    }
                }
            });
        let marker = TrackMarker::new(ms as u64, handler);
        let id = marker.id();
        *locked(&self.end_marker) = Some(id);
        self.player.add_marker(marker);
    }

    /// Serialize a player PATCH's mutations against every other PATCH for the same guild.
    ///
    /// Discord repeats `VOICE_SERVER_UPDATE` on a region change and clients PATCH once per event, so
    /// without this two voice handshakes race and whichever finishes last wins the slot: a stale
    /// connection displaces a fresh one and the guild goes silent. The same holds for a play racing
    /// a stop or a seek.
    ///
    /// The caller resolves tracks before taking this, so it is never held across a network load, and
    /// nothing called while the guard is held may re-acquire it.
    pub async fn lock_patch(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.patch_lock.lock().await
    }

    /// The live-lyrics handle, if the lyrics feature is enabled.
    pub fn lyrics(&self) -> Option<&Arc<PlayerLyrics>> {
        self.lyrics.as_ref()
    }

    /// Subscribe to live lyrics with the given `skipTrackSource` mode, seeding whatever is playing
    /// now. Returns `false` when lyrics are disabled.
    pub fn subscribe_lyrics(&self, skip_track_source: bool) -> bool {
        let Some(lyrics) = &self.lyrics else {
            return false;
        };
        lyrics.subscribe(skip_track_source);
        if let Some(track) = self.shared.current_track() {
            lyrics.seed(query_from_info(&track.info));
        }
        true
    }

    /// Unsubscribe from live lyrics. Returns `false` when lyrics are disabled.
    pub fn unsubscribe_lyrics(&self) -> bool {
        let Some(lyrics) = &self.lyrics else {
            return false;
        };
        lyrics.unsubscribe();
        true
    }

    /// Stop playback and tear the voice connection down.
    pub fn destroy(&self) {
        self.stop_update_task();
        if let Some(lyrics) = &self.lyrics {
            lyrics.unsubscribe();
        }
        self.player.stop_track();
        if let Some(connection) = locked(&self.voice).take() {
            connection.disconnect();
        }
        let mut slots = locked(&self.shared.tracks);
        slots.current = None;
        slots.previous = None;
    }
}

mod events;
pub(crate) mod loss_counter;
mod markers;
mod transition;
pub mod update;
pub mod voice;
