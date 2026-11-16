use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use player::buffer::FrameConsumer;
use player::effects::CrossfadeOptions;
use player::playback::{
    Player, PlayerConfig, PlayerError, PlayerEvents, PlayerGuard, spawn_player,
};
use player::source::{SourceReference, Sources};
use player::track::Track;
use player::track::codec::TrackCodecs;
use voice::VoiceTarget;
use voice::ids::{ChannelId, GuildId as VoiceGuildId, UserId};

use super::frame_stats::{FrameCounter, FrameWindow};
use super::resume::{Placeholders, Resume};
use super::voice_bridge::{VoiceBridge, VoiceConnection};
use crate::session::SessionStore;
use crate::utils::locked;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GuildId(u64);

impl GuildId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for GuildId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for GuildId {
    type Err = std::num::ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

pub(crate) struct PlayerEntry {
    guild_id: GuildId,
    player: Player,
    codecs: Arc<TrackCodecs>,
    sources: Sources,
    guard: Mutex<Option<PlayerGuard>>,
    frames: Mutex<Option<FrameConsumer>>,
    pub(super) events: Mutex<Option<PlayerEvents>>,
    pub(super) event_task: Mutex<Option<JoinHandle<()>>>,
    voice: Mutex<VoiceState>,
    filters: Mutex<Map<String, Value>>,
    crossfade: Mutex<Option<CrossfadeOptions>>,
    next_track: Mutex<Option<Track>>,
    placeholders: Mutex<Placeholders>,
    pub(super) frame_counter: Arc<FrameCounter>,
    connected: Arc<AtomicBool>,
    playing: AtomicBool,
    ping: Arc<AtomicI64>,
    voice_connection: AsyncMutex<Option<VoiceConnection>>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VoiceState {
    pub(super) token: String,
    pub(super) endpoint: String,
    pub(super) session_id: String,
    pub(super) channel_id: Option<String>,
}

impl PlayerEntry {
    fn new(
        guild_id: GuildId,
        sources: Sources,
        codecs: Arc<TrackCodecs>,
        config: PlayerConfig,
    ) -> Result<Arc<Self>, PlayerError> {
        let (player, frames, events, guard) = spawn_player(sources.clone(), config)?;
        let mut filters = Map::new();
        filters.insert(String::from("pluginFilters"), Value::Object(Map::new()));
        Ok(Arc::new(Self {
            guild_id,
            player,
            codecs,
            sources,
            guard: Mutex::new(Some(guard)),
            frames: Mutex::new(Some(frames)),
            events: Mutex::new(Some(events)),
            event_task: Mutex::new(None),
            voice: Mutex::new(VoiceState::default()),
            filters: Mutex::new(filters),
            crossfade: Mutex::new(config.crossfade),
            next_track: Mutex::new(None),
            placeholders: Mutex::new(Placeholders::default()),
            frame_counter: Arc::new(FrameCounter::new()),
            connected: Arc::new(AtomicBool::new(false)),
            playing: AtomicBool::new(false),
            ping: Arc::new(AtomicI64::new(-1)),
            voice_connection: AsyncMutex::new(None),
        }))
    }

    pub(super) const fn guild_id(&self) -> GuildId {
        self.guild_id
    }

    pub(crate) fn player(&self) -> &Player {
        &self.player
    }

    pub(super) fn codecs(&self) -> &TrackCodecs {
        &self.codecs
    }

    pub(super) fn codec_registry(&self) -> Arc<TrackCodecs> {
        Arc::clone(&self.codecs)
    }

    pub(super) fn voice(&self) -> VoiceState {
        locked(&self.voice).clone()
    }

    pub(super) fn set_voice(&self, voice: VoiceState) {
        *locked(&self.voice) = voice;
    }

    pub(super) fn filters(&self) -> Map<String, Value> {
        locked(&self.filters).clone()
    }

    pub(super) fn set_filters(&self, filters: Map<String, Value>) {
        *locked(&self.filters) = filters;
    }

    pub(super) fn crossfade(&self) -> Option<CrossfadeOptions> {
        *locked(&self.crossfade)
    }

    pub(super) fn set_crossfade(&self, options: Option<CrossfadeOptions>) {
        *locked(&self.crossfade) = options;
    }

    pub(super) fn take_next_track(&self) -> Option<Track> {
        locked(&self.next_track).take()
    }

    pub(super) fn set_next_track(&self, track: Option<Track>) {
        *locked(&self.next_track) = track;
    }

    pub(super) fn next_track(&self) -> Option<Track> {
        locked(&self.next_track).as_ref().map(Track::make_clone)
    }

    pub(super) fn sources(&self) -> &Sources {
        &self.sources
    }

    /// Remember that this track opened as generated silence, and the retry it owes.
    pub(super) fn mark_placeholder(&self, track: &Track) {
        locked(&self.placeholders).mark_playing(track);
    }

    /// Remember a prepared successor that opened as generated silence.
    pub(super) fn arm_placeholder(&self, track: &Track) {
        locked(&self.placeholders).arm_next(track);
    }

    /// The armed successor started; give it the playing slot before its start is reported.
    pub(super) fn promote_placeholder(&self, reference: &SourceReference) -> bool {
        locked(&self.placeholders).promote(reference)
    }

    pub(super) fn is_placeholder(&self, reference: &SourceReference) -> bool {
        locked(&self.placeholders).is_playing(reference)
    }

    /// Take the one pending retry. Only the first caller gets the track.
    pub(super) fn take_resume(&self) -> Option<Resume> {
        locked(&self.placeholders).take_resume()
    }

    pub(super) fn resume_current(&self, generation: u64) -> bool {
        locked(&self.placeholders).is_generation(generation)
    }

    /// Real media replaced the silence; let later events for the track through again.
    pub(super) fn finish_resume(&self, generation: u64) {
        locked(&self.placeholders).finish(generation);
    }

    pub(super) fn clear_placeholder(&self, reference: &SourceReference) {
        locked(&self.placeholders).clear_playing(reference);
    }

    /// The stored successor was dropped before it started.
    pub(super) fn clear_next_placeholder(&self) {
        locked(&self.placeholders).clear_next();
    }

    #[cfg(test)]
    pub(crate) fn take_frame_consumer(&self) -> Option<FrameConsumer> {
        locked(&self.frames).take()
    }

    pub(super) fn connection_state(&self) -> (bool, i64) {
        (self.connected.load(Ordering::Acquire), self.ping.load(Ordering::Acquire))
    }

    pub(super) async fn connect_voice(
        self: &Arc<Self>,
        channel_id: u64,
        user_id: u64,
        voice: VoiceState,
        sessions: Arc<SessionStore>,
        session_id: String,
    ) -> Result<bool, String> {
        if self.voice() == voice && self.connected.load(Ordering::Acquire) {
            return Ok(false);
        }
        let previous = self.voice_connection.lock().await.take();
        let frames = match previous {
            Some(connection) => connection.close().await,
            None => locked(&self.frames).take(),
        }
        .ok_or_else(|| String::from("player frame source is unavailable"))?;
        let target = VoiceTarget {
            endpoint: voice.endpoint.clone(),
            guild: VoiceGuildId(self.guild_id.get()),
            channel: ChannelId(channel_id),
            user: UserId(user_id),
            session_id: voice.session_id.clone(),
            token: voice.token.clone(),
        };
        let log_session_id = session_id.clone();
        match VoiceConnection::connect(
            target,
            frames,
            VoiceBridge {
                connected: Arc::clone(&self.connected),
                ping: Arc::clone(&self.ping),
                sessions,
                session_id,
                player: self.player.clone(),
                frame_counter: Arc::clone(&self.frame_counter),
            },
        )
        .await
        {
            Ok(connection) => {
                *self.voice_connection.lock().await = Some(connection);
                self.set_voice(voice);
                Ok(true)
            }
            Err((error, frames)) => {
                tracing::warn!(
                    session_id = %log_session_id,
                    guild_id = %self.guild_id,
                    %error,
                    "voice connection failed"
                );
                *locked(&self.frames) = Some(*frames);
                self.connected.store(false, Ordering::Release);
                self.ping.store(-1, Ordering::Release);
                Err(error)
            }
        }
    }

    pub(super) async fn notify_voice_frames(&self) {
        let frames = self.voice_connection.lock().await.as_ref().map(VoiceConnection::frames);
        if let Some(frames) = frames {
            let _ = frames.notify().await;
        }
    }

    pub(crate) fn set_playing(&self, playing: bool) -> bool {
        let previous = self.playing.swap(playing, Ordering::AcqRel);
        if previous != playing {
            self.frame_counter.set_playing(playing);
        }
        previous
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }

    pub(crate) async fn shutdown(&self) -> Result<(), PlayerError> {
        let connection = self.voice_connection.lock().await.take();
        if let Some(connection) = connection
            && let Some(frames) = connection.close().await
        {
            *locked(&self.frames) = Some(frames);
        }
        let guard = locked(&self.guard).take();
        let result = match guard {
            Some(guard) => guard.shutdown().await,
            None => Ok(()),
        };
        let event_task = locked(&self.event_task).take();
        if let Some(event_task) = event_task {
            let _ = event_task.await;
        }
        result
    }
}

#[derive(Default)]
pub(crate) struct PlayerRegistry {
    entries: player::manager::PlayerRegistry<GuildId, PlayerEntry>,
}

impl PlayerRegistry {
    pub(crate) fn get(&self, guild_id: GuildId) -> Option<Arc<PlayerEntry>> {
        self.entries.get(guild_id)
    }

    pub(crate) fn list(&self) -> Vec<Arc<PlayerEntry>> {
        self.entries.list()
    }

    pub(crate) fn create(
        &self,
        guild_id: GuildId,
        sources: Sources,
        codecs: Arc<TrackCodecs>,
        config: PlayerConfig,
    ) -> Result<Arc<PlayerEntry>, PlayerError> {
        self.entries.get_or_try_insert_with(guild_id, || {
            PlayerEntry::new(guild_id, sources, codecs, config)
        })
    }

    pub(crate) fn remove(&self, guild_id: GuildId) -> Option<Arc<PlayerEntry>> {
        self.entries.remove(guild_id)
    }

    pub(crate) fn drain(&self) -> Vec<Arc<PlayerEntry>> {
        self.entries.drain()
    }

    pub(crate) fn frame_windows(&self) -> Vec<FrameWindow> {
        self.list().into_iter().filter_map(|player| player.frame_window()).collect()
    }
}
