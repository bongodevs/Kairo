use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::ALLOW;
use axum::http::{HeaderName, HeaderValue, StatusCode, Version};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use tokio::net::TcpListener;

use player::buffer::FrameBufferConfig;
use player::playback::PlayerConfig;
use player::source::{
    ExtendedTrackCodec, HTTP_SOURCE, LOCAL_SOURCE, LocalAddressProvider, ProviderManager, Sources,
    local_address_provider,
};
use player::track::codec::TrackCodecs;

use crate::config::{Config, RequestLoggingConfig, resolve_providers};
use crate::error;
use crate::players::FilterPolicy;
use crate::plugins::PluginManager;
use crate::rest::middleware::auth;
use crate::rest::middleware::request_log;
use crate::rest::routes::{
    info, players, plugins as plugin_routes, route_planner, sessions, stats, tracks,
};
use crate::route_planner::RoutePlanner;
use crate::session::SessionStore;
use crate::ws;
const API_VERSION: &str = "4";

pub(crate) const MAX_REQUEST_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) password: Option<String>,
    pub(crate) start: Instant,
    pub(crate) cores: u32,
    pub(crate) cpu: Arc<CpuSampler>,
    pub(crate) sources: Arc<Sources>,
    pub(crate) codecs: Arc<TrackCodecs>,
    pub(crate) filter_policy: FilterPolicy,
    pub(crate) player_config: PlayerConfig,
    pub(crate) player_update_interval: Duration,
    pub(crate) sessions: Arc<SessionStore>,
    pub(crate) route_planner: Option<Arc<RoutePlanner>>,
    pub(crate) metrics_endpoint: Option<String>,
    pub(crate) http2_enabled: bool,
    pub(crate) search_metrics: Arc<crate::stats::SearchMetrics>,
    pub(crate) request_log: RequestLoggingConfig,
    pub(crate) shutdown: tokio::sync::watch::Sender<bool>,
    pub(crate) plugins: Arc<PluginManager>,
}

pub(crate) struct CpuSampler {
    pub(crate) load: Mutex<Option<(u64, u64)>>,
}

impl AppState {
    pub(crate) fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    pub(crate) fn sources(&self) -> &Sources {
        &self.sources
    }

    pub(crate) fn plugins(&self) -> &PluginManager {
        &self.plugins
    }

    pub(crate) fn codecs(&self) -> &TrackCodecs {
        &self.codecs
    }

    pub(crate) fn codec_registry(&self) -> Arc<TrackCodecs> {
        Arc::clone(&self.codecs)
    }

    pub(crate) const fn filter_policy(&self) -> FilterPolicy {
        self.filter_policy
    }

    pub(crate) const fn player_config(&self) -> PlayerConfig {
        self.player_config
    }

    pub(crate) const fn player_update_interval(&self) -> Duration {
        self.player_update_interval
    }
}

pub async fn serve(config: &Config) -> anyhow::Result<()> {
    let cores = std::thread::available_parallelism().map_or(1, |count| count.get() as u32);
    let metrics_endpoint = config
        .metrics
        .prometheus
        .enabled
        .then(|| config.metrics.prometheus.endpoint.trim_start().to_owned());
    if let Some(path) = &metrics_endpoint
        && !path.starts_with('/')
    {
        anyhow::bail!(
            "metrics.prometheus.endpoint {:?} must start with `/`",
            config.metrics.prometheus.endpoint
        );
    }
    let (shutdown, stop_seen) = tokio::sync::watch::channel(false);
    let notice = shutdown.clone();
    let route_planner =
        RoutePlanner::build(config.lavalink.server.ratelimit.as_ref())?.map(Arc::new);
    let player_config = player_config(config)?;
    let player_update_interval = Duration::from_secs(config.lavalink.server.player_update_interval);
    if player_update_interval.is_zero() {
        anyhow::bail!("lavalink.server.playerUpdateInterval must be greater than zero");
    }
    let ip: IpAddr = config.server.address.parse().with_context(|| {
        format!("server.address {:?} is not an IP address", config.server.address)
    })?;
    let addr = SocketAddr::new(ip, config.server.port);
    let listener = TcpListener::bind(addr).await.with_context(|| format!("bind {addr}"))?;
    let bound = listener.local_addr().context("read bound address")?;
    let plugins = Arc::new(
        PluginManager::load(&config.plugins, &config.plugin_directory.directory)
            .await
            .context("configure plugins")?,
    );
    let sources = match build(config, route_planner.as_ref(), &plugins) {
        Ok(sources) => sources,
        Err(error) => {
            plugins.shutdown().await;
            return Err(error);
        }
    };
    let (loaded_plugins, failed_plugins) = plugins.counts();
    tracing::info!(
        plugins = ?plugins.names().collect::<Vec<_>>(),
        loaded = loaded_plugins,
        failed = failed_plugins,
        "plugins configured"
    );
    let codecs = sources.codecs_arc();
    tracing::info!(
        sources = ?sources.names().collect::<Vec<_>>(),
        "audio sources configured"
    );
    let state = AppState {
        password: config.lavalink.server.password.clone(),
        start: Instant::now(),
        cores,
        cpu: Arc::new(CpuSampler { load: Mutex::new(None) }),
        sources,
        codecs,
        filter_policy: FilterPolicy::configured(&config.lavalink.server.filters),
        player_config,
        player_update_interval,
        sessions: Arc::new(SessionStore::default()),
        route_planner,
        metrics_endpoint,
        http2_enabled: config.server.http2.enabled,
        search_metrics: Arc::new(crate::stats::SearchMetrics::default()),
        request_log: config.logging.request.clone(),
        shutdown,
        plugins: Arc::clone(&plugins),
    };
    let sessions = state.sessions.clone();
    let app = router(state);

    tracing::info!(
        address = %bound,
        api_version = API_VERSION,
        auth = config.lavalink.server.password.is_some(),
        rest = "/v4",
        websocket = "/v4/websocket",
        "REST and WebSocket server listening"
    );

    let serve = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            await_stop_signal().await;
            tracing::info!("shutdown signal received, closing client sessions");
            let _ = notice.send(true);
        })
        .await;
    if *stop_seen.borrow() {
        sessions.wait_until_idle().await;
        tracing::info!("all client sessions drained");
    }
    plugins.shutdown().await;
    serve.context("serve http")
}

pub(crate) fn router(state: AppState) -> Router {
    let log_requests = state.request_log.enabled;
    let mut rest = Router::new()
        .route("/", get(old_websocket).options(preflight("GET,HEAD,OPTIONS")))
        .route("/v3/websocket", get(old_websocket).options(preflight("GET,HEAD,OPTIONS")))
        .route("/version", get(info::version).options(preflight("GET,HEAD,OPTIONS")))
        .route("/v4/info", get(info::info).options(preflight("GET,HEAD,OPTIONS")))
        .route("/v4/stats", get(stats::stats).options(preflight("GET,HEAD,OPTIONS")))
        .route("/v4/plugins", get(plugin_routes::list).options(preflight("GET,HEAD,OPTIONS")))
        .route(
            "/v4/plugins/{name}/reload",
            post(plugin_routes::reload).options(preflight("POST,OPTIONS")),
        )
        .route("/v4/loadtracks", get(tracks::loadtracks).options(preflight("GET,HEAD,OPTIONS")))
        .route("/v4/decodetrack", get(tracks::decodetrack).options(preflight("GET,HEAD,OPTIONS")))
        .route("/v4/decodetracks", post(tracks::decodetracks).options(preflight("POST,OPTIONS")))
        .route(
            "/v4/routeplanner/status",
            get(route_planner::status).options(preflight("GET,HEAD,OPTIONS")),
        )
        .route(
            "/v4/routeplanner/free/address",
            post(route_planner::free_address).options(preflight("POST,OPTIONS")),
        )
        .route(
            "/v4/routeplanner/free/all",
            post(route_planner::free_all).options(preflight("POST,OPTIONS")),
        )
        .route(
            "/v4/sessions/{session_id}/players",
            get(players::get_players).options(preflight("GET,HEAD,OPTIONS")),
        )
        .route(
            "/v4/sessions/{session_id}/players/{guild_id}",
            get(players::get_player)
                .patch(players::patch_player)
                .delete(players::delete_player)
                .options(preflight("GET,HEAD,PATCH,DELETE,OPTIONS")),
        )
        .route(
            "/v4/sessions/{session_id}",
            patch(sessions::patch_session).options(preflight("PATCH,OPTIONS")),
        )
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY))
        .layer(middleware::from_fn_with_state(state.clone(), auth::middleware));
    if log_requests {
        rest = rest.layer(middleware::from_fn_with_state(state.clone(), request_log::middleware));
    }

    let rest = rest.layer(middleware::from_fn(error::middleware));

    let mut outer = Router::new().route("/v4/websocket", get(ws::websocket));
    if let Some(path) = state.metrics_endpoint.clone() {
        outer = outer.route(&path, get(crate::stats::prometheus));
    }
    if log_requests {
        outer = outer.layer(middleware::from_fn_with_state(state.clone(), request_log::middleware));
    }
    outer
        .merge(rest)
        .layer(middleware::from_fn_with_state(state.clone(), http2_gate))
        .layer(middleware::from_fn(api_version))
        .with_state(state)
}

async fn http2_gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.version() == Version::HTTP_2 && !state.http2_enabled {
        tracing::warn!("rejecting an HTTP/2 request because server.http2.enabled is false");
        return StatusCode::HTTP_VERSION_NOT_SUPPORTED.into_response();
    }
    next.run(req).await
}

async fn old_websocket() {
    tracing::warn!(
        "This is the old Lavalink websocket endpoint. Please use /v4/websocket instead. If you are \
         using a client library, please update it to a Lavalink v4 compatible version or use \
         Lavalink v3 instead."
    );
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

fn preflight(methods: &'static str) -> impl Fn() -> std::future::Ready<Response> + Clone {
    move || {
        let mut response = StatusCode::OK.into_response();
        response.headers_mut().insert(ALLOW, HeaderValue::from_static(methods));
        std::future::ready(response)
    }
}

async fn api_version(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        HeaderName::from_static("lavalink-api-version"),
        HeaderValue::from_static(API_VERSION),
    );
    response
}

async fn await_stop_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => log_signal(result, "ctrl-C"),
                    Some(_) = terminate.recv() => log_signal(Ok(()), "SIGTERM"),
                }
                return;
            }
            Err(err) => tracing::warn!(%err, "cannot watch SIGTERM, stopping only on ctrl-C"),
        }
    }
    let result = tokio::signal::ctrl_c().await;
    log_signal(result, "ctrl-C");
}

fn log_signal(result: std::io::Result<()>, name: &str) {
    match result {
        Ok(()) => {}
        Err(err) => tracing::warn!(%err, "{name} handler failed; shutting down anyway"),
    }
}

fn player_config(config: &Config) -> anyhow::Result<PlayerConfig> {
    let server = &config.lavalink.server;
    if let Some(duration) = server.buffer_duration_ms {
        tracing::warn!(
            value = duration,
            "bufferDurationMs sizes the reference's native sender queue; Kairo has no equivalent \
             queue, so the value is ignored"
        );
    }
    Ok(PlayerConfig {
        frame_buffer: FrameBufferConfig::new(server.frame_buffer_duration_ms)
            .context("configure player frame buffer")?,
        resampling_quality: server.resampling_quality.map_or_else(Default::default, |quality| {
            match quality {
                crate::config::ResamplingQualityConfig::Low => player::pcm::ResamplingQuality::Low,
                crate::config::ResamplingQualityConfig::Medium => {
                    player::pcm::ResamplingQuality::Medium
                }
                crate::config::ResamplingQualityConfig::High => {
                    player::pcm::ResamplingQuality::High
                }
            }
        }),
        opus: player::opus::OpusEncoderConfig {
            complexity: server.opus_quality(),
            ..player::opus::OpusEncoderConfig::default()
        },
        track_stuck_threshold_ms: server.track_stuck_threshold_ms,
        use_seek_ghosting: server.use_seek_ghosting,
        crossfade: config.crossfade.to_engine(),
        ..PlayerConfig::default()
    })
}

fn build(
    config: &Config,
    route_planner: Option<&Arc<RoutePlanner>>,
    plugins: &PluginManager,
) -> anyhow::Result<Arc<Sources>> {
    let timeouts = config.lavalink.server.timeouts;
    let local_address: Option<LocalAddressProvider> = route_planner.map(|planner| {
        let planner = Arc::clone(planner);
        local_address_provider(move || planner.current_address())
    });
    let mut providers = ProviderManager::new()
        .with_probe(player::source_probe::audio_probe())
        .with_http_timeouts(
            Duration::from_millis(timeouts.connect_timeout_ms),
            Duration::from_millis(timeouts.socket_timeout_ms),
        )
        .with_http_proxy(
            config.lavalink.server.http_config.as_ref().and_then(|proxy| proxy.to_proxy()),
        )
        .with_local_address(local_address);
    let source_config = config.lavalink.server.sources.source_config(&config.sources);
    let plugin_sources = plugins.source_names();
    let fallbacks = resolve_providers(
        &config.sources.providers,
        plugin_sources.iter().map(String::as_str).chain(
            [(source_config.local, LOCAL_SOURCE), (source_config.http, HTTP_SOURCE)]
                .into_iter()
                .filter_map(|(enabled, name)| enabled.then_some(name)),
        ),
    )
    .context("validate sources.providers")?;
    let mut reserved = vec![String::from(HTTP_SOURCE), String::from(LOCAL_SOURCE)];
    reserved.extend(providers.names().map(str::to_owned));
    let mirror = plugins
        .register_sources(&mut providers, reserved.iter().map(String::as_str), fallbacks)
        .context("register plugin sources")?;

    providers.set_fallback_codec(ExtendedTrackCodec);
    let sources = Arc::new(providers.build(source_config).context("configure audio sources")?);
    mirror.install(&sources);
    Ok(sources)
}

#[cfg(test)]
#[path = "tests/audio_sources.rs"]
mod audio_sources_tests;
