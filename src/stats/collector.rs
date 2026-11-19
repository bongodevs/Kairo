use std::sync::atomic::Ordering;

use serde::Serialize;
use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

use crate::app::AppState;
use crate::players::{FrameStats, summarize};
use crate::utils::locked;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub(super) struct Memory {
    pub(super) free: u64,
    pub(super) used: u64,
    pub(super) allocated: u64,
    pub(super) reservable: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Cpu {
    pub(super) cores: u32,
    pub(super) system_load: f64,
    pub(super) lavalink_load: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Stats {
    pub(super) players: u32,
    pub(super) playing_players: u32,
    pub(super) uptime: u64,
    pub(super) memory: Memory,
    pub(super) cpu: Cpu,
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

fn memory(total: u64, available: u64) -> Memory {
    Memory {
        free: available,
        used: total.saturating_sub(available),
        allocated: total,
        reservable: total,
    }
}

fn load_fraction(previous: Option<(u64, u64)>, now: (u64, u64), cores: u32) -> f64 {
    let Some((cpu_before, mono_before)) = previous else {
        return 0.0;
    };
    let wall_ms = now.1.saturating_sub(mono_before);
    if wall_ms == 0 {
        return 0.0;
    }
    clamp01(now.0.saturating_sub(cpu_before) as f64 / (wall_ms as f64 * f64::from(cores.max(1))))
}

#[derive(Debug, Default)]
struct Metrics {
    total: u64,
    available: u64,
    load_one: f64,
    process_load: f64,
}

const MIN_SAMPLE_INTERVAL_MS: u64 = 250;

fn read_metrics(state: &AppState) -> Metrics {
    let Ok(mut system) = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    ) else {
        return Metrics::default();
    };
    let pid = Pid::from(std::process::id() as usize);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu(),
    );
    let cpu_ms = system.process(pid).map_or(0, |process| process.accumulated_cpu_time());
    let mono_ms = state.start.elapsed().as_millis() as u64;

    let mut sample = locked(&state.cpu.state);
    let process_load = if sample.sampled_at > 0
        && mono_ms.saturating_sub(sample.sampled_at) < MIN_SAMPLE_INTERVAL_MS
    {
        f64::from_bits(state.cpu.process_load.load(Ordering::Acquire))
    } else {
        let load = load_fraction(sample.baseline, (cpu_ms, mono_ms), state.cores);
        sample.baseline = Some((cpu_ms, mono_ms));
        sample.sampled_at = mono_ms;
        state.cpu.process_load.store(load.to_bits(), Ordering::Release);
        load
    };

    Metrics {
        total: system.total_memory(),
        available: system.available_memory(),
        load_one: System::load_average().map_or(0.0, |load| load.one),
        process_load,
    }
}

pub(crate) async fn snapshot(state: &AppState) -> Stats {
    let uptime = state.start.elapsed().as_millis() as u64;
    let reading = state.clone();
    let metrics =
        tokio::task::spawn_blocking(move || read_metrics(&reading)).await.unwrap_or_default();
    let (players, playing_players) = state.sessions.node_totals();

    Stats {
        players,
        playing_players,
        uptime,
        memory: memory(metrics.total, metrics.available),
        cpu: Cpu {
            cores: state.cores,
            system_load: clamp01(metrics.load_one / f64::from(state.cores.max(1))),
            lavalink_load: metrics.process_load,
        },
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsFrame {
    op: &'static str,
    frame_stats: Option<FrameStats>,
    #[serde(flatten)]
    stats: Stats,
}

pub(crate) async fn ws_stats(state: &AppState, session_id: &str) -> Option<String> {
    let frame = StatsFrame {
        op: "stats",
        frame_stats: state
            .sessions
            .frame_windows(session_id)
            .and_then(|windows| summarize(&windows)),
        stats: snapshot(state).await,
    };
    match serde_json::to_string(&frame) {
        Ok(text) => Some(text),
        Err(err) => {
            tracing::warn!(?err, "failed to serialize the stats frame");
            None
        }
    }
}

#[cfg(test)]
#[path = "../tests/stats.rs"]
mod tests;
