//! Outbound WebSocket messages.
//!
//! These only ever travel to the client, so they derive [`Serialize`] alone. The wire shape is
//! internally tagged on `op`, one of `ready`, `playerUpdate`, `stats` or `event`, and an `event` is
//! tagged again on `type`.

use serde::Serialize;

use crate::protocol::load_result::Exception;
use crate::protocol::lyrics::{Line, Lyrics};
use crate::protocol::player::PlayerState;
use crate::protocol::sponsorblock::{Chapter, Segment};
use crate::protocol::stats::{FrameStats, Stats};
use crate::protocol::track::Track;

/// A message sent to a connected client over the WebSocket.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op")]
pub enum Message {
    /// First message after the socket opens.
    #[serde(rename = "ready", rename_all = "camelCase")]
    Ready {
        /// Whether this connection resumed a previous session.
        resumed: bool,
        /// The session id to use for REST calls.
        session_id: String,
    },
    /// Periodic player state.
    #[serde(rename = "playerUpdate", rename_all = "camelCase")]
    PlayerUpdate {
        /// The guild id.
        guild_id: String,
        /// The current player state.
        state: PlayerState,
    },
    /// Periodic node statistics.
    ///
    /// `frameStats` is hoisted out of the inner [`Stats`] because this op always encodes it, as
    /// `null` when the node has no players, while the `GET /v4/stats` body leaves it out entirely.
    /// Build it through [`Message::stats`], which keeps the two in step.
    #[serde(rename = "stats", rename_all = "camelCase")]
    Stats {
        /// Frame statistics, `null` when the node has no players.
        frame_stats: Option<FrameStats>,
        /// Everything else in the stats payload.
        #[serde(flatten)]
        stats: Stats,
    },
    /// A player event. Boxed, since events are far larger than the other variants.
    #[serde(rename = "event")]
    Event(Box<EmittedEvent>),
}

impl Message {
    /// The wire `op` name, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Message::Ready { .. } => "ready",
            Message::PlayerUpdate { .. } => "playerUpdate",
            Message::Stats { .. } => "stats",
            Message::Event(_) => "event",
        }
    }

    /// Wrap an [`EmittedEvent`] as an `op: "event"` message.
    pub fn event(event: EmittedEvent) -> Self {
        Message::Event(Box::new(event))
    }

    /// Wrap node statistics as an `op: "stats"` message, hoisting `frameStats` out of `stats` so it
    /// is encoded exactly once.
    pub fn stats(mut stats: Stats) -> Self {
        let frame_stats = stats.frame_stats.take();
        Message::Stats { frame_stats, stats }
    }
}

/// A player event, tagged on `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum EmittedEvent {
    /// A track started playing.
    #[serde(rename = "TrackStartEvent", rename_all = "camelCase")]
    TrackStart {
        /// The guild id.
        guild_id: String,
        /// The track that started.
        track: Track,
    },
    /// A pre-buffered successor became the active track. The server is already playing it, so a
    /// client advances its queue state and re-anchors position without sending a play request.
    #[serde(rename = "TrackPromotedEvent", rename_all = "camelCase")]
    TrackPromoted {
        /// The guild id.
        guild_id: String,
        /// The track that is now active.
        track: Track,
    },
    /// A track ended.
    #[serde(rename = "TrackEndEvent", rename_all = "camelCase")]
    TrackEnd {
        /// The guild id.
        guild_id: String,
        /// The track that ended.
        track: Track,
        /// Why the track ended.
        reason: TrackEndReason,
    },
    /// A track threw an exception, which may or may not be fatal.
    #[serde(rename = "TrackExceptionEvent", rename_all = "camelCase")]
    TrackException {
        /// The guild id.
        guild_id: String,
        /// The affected track.
        track: Track,
        /// The exception.
        exception: Exception,
    },
    /// A track got stuck, producing no frames within the threshold.
    #[serde(rename = "TrackStuckEvent", rename_all = "camelCase")]
    TrackStuck {
        /// The guild id.
        guild_id: String,
        /// The affected track.
        track: Track,
        /// The stuck threshold in milliseconds.
        threshold_ms: i64,
    },
    /// The Discord voice WebSocket closed.
    #[serde(rename = "WebSocketClosedEvent", rename_all = "camelCase")]
    WebSocketClosed {
        /// The guild id.
        guild_id: String,
        /// The Discord voice close code.
        code: i32,
        /// The close reason.
        reason: String,
        /// Whether Discord initiated the close.
        by_remote: bool,
    },
    /// A SponsorBlock segment was skipped.
    #[serde(rename = "SegmentSkipped", rename_all = "camelCase")]
    SegmentSkipped {
        /// The guild id.
        guild_id: String,
        /// The segment that was skipped.
        segment: Segment,
    },
    /// SponsorBlock segments were loaded for a track.
    #[serde(rename = "SegmentsLoaded", rename_all = "camelCase")]
    SegmentsLoaded {
        /// The guild id.
        guild_id: String,
        /// The loaded segments.
        segments: Vec<Segment>,
    },
    /// A video chapter started.
    #[serde(rename = "ChapterStarted", rename_all = "camelCase")]
    ChapterStarted {
        /// The guild id.
        guild_id: String,
        /// The chapter that started.
        chapter: Chapter,
    },
    /// Video chapters were loaded for a track.
    #[serde(rename = "ChaptersLoaded", rename_all = "camelCase")]
    ChaptersLoaded {
        /// The guild id.
        guild_id: String,
        /// The loaded chapters.
        chapters: Vec<Chapter>,
    },
    /// Lyrics were found for the current track.
    #[serde(rename = "LyricsFoundEvent", rename_all = "camelCase")]
    LyricsFound {
        /// The guild id.
        guild_id: String,
        /// The resolved lyrics.
        lyrics: Lyrics,
    },
    /// No lyrics were found for the current track.
    #[serde(rename = "LyricsNotFoundEvent", rename_all = "camelCase")]
    LyricsNotFound {
        /// The guild id.
        guild_id: String,
    },
    /// A synced lyric line was reached during playback.
    #[serde(rename = "LyricsLineEvent", rename_all = "camelCase")]
    LyricsLine {
        /// The guild id.
        guild_id: String,
        /// The index of the line within the lyrics.
        line_index: i32,
        /// The line that was reached.
        line: Line,
        /// Whether the line was passed over, by a seek or by arriving late, rather than reached in
        /// real time.
        skipped: bool,
    },
}

/// Why a track stopped.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackEndReason {
    /// The track reached its end, or an end marker stopped it.
    Finished,
    /// The track failed to start, throwing before producing audio.
    LoadFailed,
    /// The player was stopped.
    Stopped,
    /// A new track replaced this one.
    Replaced,
    /// The player was cleaned up after the idle threshold.
    Cleanup,
    /// The track was crossfaded into its successor, which is already playing, so a client advances
    /// its queue state without sending a new play request.
    Crossfade,
    /// The track was handed off gaplessly to a pre-buffered successor, which is already playing.
    Gapless,
}

impl TrackEndReason {
    /// Map an engine end reason to the wire reason.
    pub fn from_player(reason: player::track::state::AudioTrackEndReason) -> Self {
        use player::track::state::AudioTrackEndReason as R;
        match reason {
            R::Finished => TrackEndReason::Finished,
            R::LoadFailed => TrackEndReason::LoadFailed,
            R::Stopped => TrackEndReason::Stopped,
            R::Replaced => TrackEndReason::Replaced,
            R::Cleanup => TrackEndReason::Cleanup,
            R::Crossfade => TrackEndReason::Crossfade,
            R::Gapless => TrackEndReason::Gapless,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::stats::{Cpu, Memory};

    fn sample_stats(frame_stats: Option<FrameStats>) -> Stats {
        Stats {
            frame_stats,
            players: 1,
            playing_players: 1,
            loader_running: 0,
            loader_timed_out: 0,
            uptime: 1000,
            memory: Memory {
                free: 1,
                used: 2,
                allocated: 3,
                reservable: 4,
            },
            cpu: Cpu {
                cores: 8,
                system_load: 0.1,
                lavalink_load: 0.2,
            },
        }
    }

    // The `stats` op has to carry `frameStats` exactly once, `null` included, and the flatten plus
    // hoist in `Message::stats` is easy to break without noticing.
    #[test]
    fn ws_stats_always_encodes_frame_stats() {
        let json = serde_json::to_string(&Message::stats(sample_stats(None))).unwrap();
        assert!(json.contains(r#""op":"stats""#), "{json}");
        assert!(json.contains(r#""frameStats":null"#), "{json}");

        let frames = FrameStats {
            sent: 3000,
            nulled: 0,
            deficit: 0,
        };
        let json = serde_json::to_string(&Message::stats(sample_stats(Some(frames)))).unwrap();
        assert_eq!(json.matches("frameStats").count(), 1, "{json}");
        assert!(json.contains(r#""sent":3000"#), "{json}");

        // The REST body leaves it out instead.
        let json = serde_json::to_string(&sample_stats(None)).unwrap();
        assert!(!json.contains("frameStats"), "{json}");
    }
}
