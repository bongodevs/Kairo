//! Node statistics.

use serde::Serialize;

/// Node statistics, sent over the WebSocket (`op: "stats"`) and via `GET /v4/stats`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Frame statistics; `null` when the node has no players or via `GET /v4/stats`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_stats: Option<FrameStats>,
    /// Number of players connected to the node.
    pub players: i32,
    /// Number of players actively playing a track.
    pub playing_players: i32,
    /// Loader slots currently in use.
    pub loader_running: usize,
    /// Loads rejected because no loader slot freed up within the wait timeout.
    pub loader_timed_out: u64,
    /// Node uptime in milliseconds.
    pub uptime: i64,
    /// Memory statistics.
    pub memory: Memory,
    /// CPU statistics.
    pub cpu: Cpu,
}

/// Per-minute frame statistics.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FrameStats {
    /// Frames sent to Discord.
    pub sent: i32,
    /// Frames nulled, meaning silence went out because no audio was ready.
    pub nulled: i32,
    /// Expected frames less the sent and nulled ones.
    pub deficit: i32,
}

/// Memory statistics.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Memory {
    /// Free memory in bytes.
    pub free: i64,
    /// Used memory in bytes.
    pub used: i64,
    /// Allocated memory in bytes.
    pub allocated: i64,
    /// Reservable memory in bytes.
    pub reservable: i64,
}

/// CPU statistics.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cpu {
    /// Number of CPU cores.
    pub cores: i32,
    /// System-wide CPU load, `0.0..=1.0`.
    pub system_load: f64,
    /// This process's CPU load, `0.0..=1.0`.
    pub lavalink_load: f64,
}
