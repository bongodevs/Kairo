//! The shared application state passed to every handler, and the background broadcast tasks.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lyrics::LyricsService;
use player::tools::http_config::{set_outbound_http_config, OutboundHttpConfig};
use player::AudioPlayerManager;

use crate::config::Config;
use crate::node::stats;
use crate::node::tasks::{self, TASKS};
use crate::protocol::message::Message;
use crate::protocol::stats::Stats;
use crate::routeplanner::IpRoutePlanner;
use crate::session::{SocketContext, SocketServer};

// How often each session is sent node `stats`.
const STATS_INTERVAL_SECS: u64 = 60;
// How often the player cleanup sweep runs.
const CLEANUP_CHECK_INTERVAL_SECS: u64 = 10;
// How often freed pages are handed back to the OS. mimalloc caches them per-arena for fast reuse,
// so without this RSS settles at the highest point playback ever reached and stays there. Spaced
// out because purging costs allocator overhead on the bursty alloc/free of decoding.
const HEAP_RECLAIM_INTERVAL_SECS: u64 = 300;
// How long a track may go without a frame request before it is ended with the `cleanup` reason.
// Not configurable: the protocol has no key for it.
const PLAYER_CLEANUP_THRESHOLD_MS: u64 = 60_000;

/// Shared, cheaply cloneable application state.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    manager: AudioPlayerManager,
    sockets: SocketServer,
    start_time: Instant,
    // Present when `lavalink.server.ratelimit` is configured.
    route_planner: Option<Arc<IpRoutePlanner>>,
    // Present when `lyrics.enabled` and at least one provider is on. `None` makes every lyrics
    // endpoint report the feature as unavailable.
    lyrics_service: Option<Arc<LyricsService>>,
}

impl AppState {
    /// Build the state from `config`: configure and populate the audio engine, install the outbound
    /// HTTP policy, then arm the player cleanup sweep. Must be called within a tokio runtime.
    pub fn new(config: Config) -> Self {
        let manager = AudioPlayerManager::new();
        manager.set_configuration(config.audio_configuration());
        manager.set_frame_buffer_duration(config.lavalink.server.frame_buffer_duration_ms);
        manager.set_use_seek_ghosting(config.lavalink.server.use_seek_ghosting);

        // Install the proxy, route planner and connect timeout before the sources, so a client one
        // of them builds while constructing already sees it.
        let route_planner = IpRoutePlanner::from_config(&config.lavalink.server.ratelimit);
        let proxy = config.lavalink.server.http_config.to_proxy();
        set_outbound_http_config(OutboundHttpConfig {
            proxy,
            route_planner: route_planner
                .clone()
                .map(|p| p as Arc<dyn player::tools::http_config::RoutePlanner>),
            connect_timeout: config.lavalink.server.timeouts.connect_timeout(),
            retry_attempts: config.lavalink.server.ratelimit.retry_attempts(),
            search_triggers_fail: config.lavalink.server.ratelimit.search_triggers_fail,
        });

        // Registration order, platform sources before the generic `http` and `local` ones, lives in
        // the `sources` crate, so adding a source never touches this file.
        config.sources.register_all(&manager);

        let lyrics_service = config.lyrics.build_service();

        if route_planner.is_some() {
            tracing::info!(
                strategy = ?config.lavalink.server.ratelimit.strategy,
                blocks = config.lavalink.server.ratelimit.ip_blocks.len(),
                "IP route planner enabled"
            );
        }
        if config.lavalink.server.http_config.is_enabled() {
            tracing::info!(
                host = %config.lavalink.server.http_config.proxy_host,
                "Outbound HTTP proxy enabled"
            );
        }

        let resumable_limit = config.lavalink.server.resumable_session_limit;
        let state = AppState {
            inner: Arc::new(Inner {
                config,
                manager,
                sockets: SocketServer::with_limit(resumable_limit),
                start_time: Instant::now(),
                route_planner,
                lyrics_service,
            }),
        };
        // Each player runs its own update timer and each session its own stats timer, so there is no
        // node-wide ticker to spawn here, only the interval to hand over.
        crate::session::player::set_update_interval(
            state.inner.config.lavalink.server.player_update_interval,
        );
        state.spawn_player_cleanup_task();
        state.spawn_resource_monitor_task();
        crate::node::runtime_metrics::spawn_runtime_metrics_task();
        spawn_heap_reclaim_task();
        state
    }

    #[cfg(test)]
    pub fn test_state() -> Self {
        let mut config = Config::default();
        config.lavalink.server.password = "youshallnotpass".to_string();
        config.metrics.prometheus.enabled = true;
        config.metrics.prometheus.endpoint = "/metrics".to_string();
        let resumable_limit = config.lavalink.server.resumable_session_limit;
        Self {
            inner: Arc::new(Inner {
                config,
                manager: AudioPlayerManager::new(),
                sockets: SocketServer::with_limit(resumable_limit),
                start_time: Instant::now(),
                route_planner: None,
                lyrics_service: None,
            }),
        }
    }

    pub fn route_planner(&self) -> Option<&Arc<IpRoutePlanner>> {
        self.inner.route_planner.as_ref()
    }

    pub fn lyrics_service(&self) -> Option<&Arc<LyricsService>> {
        self.inner.lyrics_service.as_ref()
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn manager(&self) -> &AudioPlayerManager {
        &self.inner.manager
    }

    pub fn sockets(&self) -> &SocketServer {
        &self.inner.sockets
    }

    /// Node uptime in milliseconds.
    pub fn uptime_ms(&self) -> i64 {
        self.inner.start_time.elapsed().as_millis() as i64
    }

    /// The source manager names enabled in config.
    pub fn source_names(&self) -> Vec<String> {
        self.inner.config.sources.source_names()
    }

    /// The enabled lyrics provider names, or empty when the feature is disabled.
    pub fn lyrics_provider_names(&self) -> Vec<String> {
        if self.inner.lyrics_service.is_some() {
            self.inner.config.lyrics.provider_names()
        } else {
            Vec::new()
        }
    }

    /// Build the node [`Stats`], optionally with per-session frame stats.
    pub fn build_stats(&self, frame_stats: Option<crate::protocol::stats::FrameStats>) -> Stats {
        Stats {
            frame_stats,
            players: self.inner.sockets.total_players() as i32,
            playing_players: self.inner.sockets.total_playing_players() as i32,
            loader_running: self.inner.manager.loader_running(),
            loader_timed_out: self.inner.manager.loader_timed_out(),
            uptime: self.uptime_ms(),
            memory: stats::memory(),
            cpu: stats::cpu(),
        }
    }

    /// Start this session's `stats` timer: immediately, then once a minute on a fixed grid.
    pub fn arm_session_stats(&self, context: &Arc<SocketContext>) {
        let state = self.clone();
        // Weak, so a session dropped without `shutdown()` running cannot be kept alive by its timer.
        let weak = Arc::downgrade(context);
        let handle = TASKS.add_fixed_rate(
            tasks::session_stats(&context.session_id),
            Duration::from_secs(STATS_INTERVAL_SECS),
            move || {
                let state = state.clone();
                let weak = weak.clone();
                async move {
                    let Some(context) = weak.upgrade() else {
                        return;
                    };
                    let stats = state.build_stats(context.aggregate_frame_stats());
                    context.send_message(Message::stats(stats));
                }
            },
        );
        context.set_stats_task(handle);
    }

    // Stop tracks stalled longer than PLAYER_CLEANUP_THRESHOLD_MS to prevent thread leaks.
    fn spawn_player_cleanup_task(&self) {
        let state = self.clone();
        TASKS.add(
            "player_cleanup",
            Duration::from_secs(CLEANUP_CHECK_INTERVAL_SECS),
            move || {
                let state = state.clone();
                async move {
                    for context in state.sockets().active_contexts() {
                        for player in context.players() {
                            player.check_cleanup(PLAYER_CLEANUP_THRESHOLD_MS);
                        }
                    }
                }
            },
        );
    }

    // Periodically sample file descriptors and active player counts on the blocking pool.
    fn spawn_resource_monitor_task(&self) {
        let state = self.clone();
        TASKS.add("resource_monitor", Duration::from_secs(30), move || {
            let state = state.clone();
            async move {
                let fd_count = tokio::task::spawn_blocking(crate::node::stats::fd_count)
                    .await
                    .ok()
                    .flatten();
                tracing::debug!(
                    fd_count = ?fd_count,
                    fd_limit = ?crate::node::stats::fd_limit(),
                    decode_threads = player::live_decode_threads(),
                    players = state.sockets().total_players(),
                    playing = state.sockets().total_playing_players(),
                    "resource monitor"
                );
            }
        });
    }
}

// Periodically return allocator arena pages freed since the last pass to the OS.
fn spawn_heap_reclaim_task() {
    TASKS.add(
        "heap_reclaim",
        Duration::from_secs(HEAP_RECLAIM_INTERVAL_SECS),
        || async {
            // Forced, so it purges the shared arenas and not just the calling thread's heap.
            unsafe { libmimalloc_sys::mi_collect(true) };
        },
    );
}
