mod collector;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{RawQuery, State};
use axum::http::HeaderValue;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;
pub(crate) use collector::{Stats, snapshot, ws_stats};

const EXPOSITION_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The reference's optional dev counters. They exist in every node but are only fed when the
/// Prometheus endpoint is enabled, just like the `SearchMetrics` bean.
#[derive(Default)]
pub(crate) struct SearchMetrics {
    loads: Mutex<BTreeMap<(String, String), u64>>,
    plays: Mutex<BTreeMap<(String, String), u64>>,
}

impl SearchMetrics {
    pub(crate) fn record_load(&self, source: &str, result: &str) {
        let mut loads = crate::utils::locked(&self.loads);
        *loads.entry((source.to_owned(), result.to_owned())).or_default() += 1;
    }

    pub(crate) fn record_play(&self, source: &str, guild_id: &str) {
        let mut plays = crate::utils::locked(&self.plays);
        *plays.entry((source.to_owned(), guild_id.to_owned())).or_default() += 1;
    }

    pub(crate) fn load_samples(&self) -> Vec<String> {
        let loads = crate::utils::locked(&self.loads);
        loads
            .iter()
            .map(|((source, result), count)| {
                format!(
                    "lavalink_item_load_results_total{{source=\"{}\",result=\"{}\"}} {count}",
                    label(source),
                    label(result)
                )
            })
            .collect()
    }

    pub(crate) fn play_samples(&self) -> Vec<String> {
        let plays = crate::utils::locked(&self.plays);
        plays
            .iter()
            .map(|((source, guild_id), count)| {
                format!(
                    "lavalink_tracks_played_total{{source=\"{}\",guild_id=\"{}\"}} {count}",
                    label(source),
                    label(guild_id)
                )
            })
            .collect()
    }
}

/// Escape a Prometheus label value the way the exposition format requires.
fn label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n").replace('"', "\\\"")
}

const LEVEL_NAMES: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

static LOG_EVENTS: [AtomicU64; 5] =
    [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

fn level_index(level: tracing::Level) -> usize {
    match level {
        tracing::Level::TRACE => 0,
        tracing::Level::DEBUG => 1,
        tracing::Level::INFO => 2,
        tracing::Level::WARN => 3,
        tracing::Level::ERROR => 4,
    }
}

pub struct CountingLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        LOG_EVENTS[level_index(*event.metadata().level())].fetch_add(1, Ordering::Relaxed);
    }
}

fn log_events() -> [u64; 5] {
    std::array::from_fn(|i| LOG_EVENTS[i].load(Ordering::Relaxed))
}

struct Family {
    name: &'static str,
    help: &'static str,
    kind: &'static str,
    samples: Vec<String>,
}

fn gauge(name: &'static str, help: &'static str, value: impl std::fmt::Display) -> Family {
    Family { name, help, kind: "gauge", samples: vec![format!("{name} {value}")] }
}

fn families(stats: &Stats, metrics: &SearchMetrics) -> Vec<Family> {
    let memory = [
        ("lavalink_memory_allocated_bytes", stats.memory.allocated, "Allocated memory"),
        ("lavalink_memory_free_bytes", stats.memory.free, "Free memory"),
        ("lavalink_memory_reservable_bytes", stats.memory.reservable, "Max reservable memory"),
        ("lavalink_memory_used_bytes", stats.memory.used, "Used memory"),
    ];
    let mut families = vec![
        gauge("lavalink_cpu_cores", "Number of CPU cores", stats.cpu.cores),
        gauge(
            "lavalink_cpu_lavalink_load_percentage",
            "Process CPU load as a percentage of all cores",
            stats.cpu.lavalink_load,
        ),
        gauge(
            "lavalink_cpu_system_load_percentage",
            "System CPU load as a percentage of all cores",
            stats.cpu.system_load,
        ),
    ];
    families.extend(memory.map(|(name, bytes, help)| gauge(name, help, bytes)));
    families.push(gauge(
        "lavalink_playing_players_total",
        "Number of players currently playing",
        stats.playing_players,
    ));
    families.push(gauge("lavalink_players_total", "Total players", stats.players));
    families.push(gauge(
        "lavalink_uptime_milliseconds",
        "Node uptime in milliseconds",
        stats.uptime,
    ));
    let counts = log_events();
    families.push(Family {
        name: "logback_events_total",
        help: "Number of log events written, by level",
        kind: "counter",
        samples: (0..LEVEL_NAMES.len())
            .map(|level| {
                format!(
                    "logback_events_total{{level=\"{}\"}} {}",
                    LEVEL_NAMES[level], counts[level]
                )
            })
            .collect(),
    });
    families.push(Family {
        name: "lavalink_tracks_played_total",
        help: "Total number of tracks played by source",
        kind: "counter",
        samples: metrics.play_samples(),
    });
    families.push(Family {
        name: "lavalink_item_load_results_total",
        help: "Total number of audio item load results by source and result type",
        kind: "counter",
        samples: metrics.load_samples(),
    });
    families
}

fn requested(query: &str) -> Vec<String> {
    let mut names = Vec::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if percent_decode(key) == "name[]" {
            names.push(percent_decode(value));
        }
    }
    names
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&value[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn render(families: &[Family]) -> String {
    let mut out = String::new();
    for family in families {
        out.push_str(&format!(
            "# HELP {} {}\n# TYPE {} {}\n",
            family.name, family.help, family.name, family.kind
        ));
        for sample in &family.samples {
            out.push_str(sample);
            out.push('\n');
        }
    }
    out
}

pub(crate) async fn prometheus(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Response {
    let snapshot = snapshot(&state).await;
    let all = families(&snapshot, &state.search_metrics);
    let wanted = query.as_deref().map_or_else(Vec::new, requested);
    let body = render(
        &all.into_iter()
            .filter(|family| wanted.is_empty() || wanted.iter().any(|name| *name == family.name))
            .collect::<Vec<_>>(),
    );
    ([(CONTENT_TYPE, HeaderValue::from_static(EXPOSITION_TYPE))], body).into_response()
}

#[cfg(test)]
#[path = "../tests/metrics.rs"]
mod tests;
