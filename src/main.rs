//! The `kairo` binary: load the configuration, install logging, then serve until a signal arrives.

use std::future::Future;
use std::pin::pin;
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::Router;
use hyper::body::Incoming;
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tower_service::Service;

use kairo::node::AppState;
use kairo::CONFIG;

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("kairo-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(run())
}

async fn run() -> ExitCode {
    disable_thp();

    let _ = rustls::crypto::ring::default_provider().install_default();

    LazyLock::force(&CONFIG);

    kairo::utils::banner::print(&CONFIG.logging);

    // The guard flushes what the file sink buffered when dropped, so it is held for all of `main`.
    let _logging = kairo::utils::init(&CONFIG.logging);

    log_fd_limit();

    // Fire and forget: it logs an upgrade notice if one exists and stays out of the way otherwise.
    tokio::spawn(kairo::utils::update::check(&CONFIG.logging));

    let bind = format!("{}:{}", CONFIG.server.address, CONFIG.server.port);
    let http2 = CONFIG.server.http2.enabled;

    let state = AppState::new(CONFIG.clone());

    let sources = state.source_names();
    if sources.is_empty() {
        tracing::warn!("No audio sources are enabled; track loading will return no matches");
    } else {
        tracing::info!(sources = %sources.join(", "), "Registered source managers");
    }

    let lyrics_providers = state.lyrics_provider_names();
    if !lyrics_providers.is_empty() {
        tracing::info!(
            providers = %lyrics_providers.join(", "),
            "Registered lyrics providers"
        );
    }

    let app = kairo::rest::app(state);

    let listener = match TcpListener::bind(&bind).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(%bind, error = %err, "failed to bind");
            return ExitCode::FAILURE;
        }
    };

    if http2 {
        tracing::info!(%bind, "Kairo listening (HTTP/1.1 and h2c)");
    } else {
        tracing::info!(%bind, "Kairo listening");
    }

    serve(listener, app, http2, shutdown_signal()).await;
    ExitCode::SUCCESS
}

// Opt out of transparent huge pages to prevent allocator arena memory bloating.
#[cfg(target_os = "linux")]
fn disable_thp() {
    if std::env::var_os("KAIRO_ALLOW_THP").is_some() {
        return;
    }
    // Only fails on kernels below 3.15, which have nothing to opt out of.
    unsafe { libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0) };
}

#[cfg(not(target_os = "linux"))]
fn disable_thp() {}

// Log process file descriptor limits and warn if too low for high concurrency.
#[cfg(unix)]
fn log_fd_limit() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return;
    }
    tracing::info!(limit = lim.rlim_cur, "file descriptor limit");
    if lim.rlim_cur < 16384 {
        tracing::warn!(
            limit = lim.rlim_cur,
            "file descriptor limit is low for large fleets; raise it (docker compose ulimits nofile 65536)"
        );
    }
}

#[cfg(not(unix))]
fn log_fd_limit() {}

// Accept connections with peer addresses and protocol negotiation until shutdown.
async fn serve(
    listener: TcpListener,
    app: Router,
    http2: bool,
    shutdown: impl Future<Output = ()>,
) {
    let mut sniffing = auto::Builder::new(TokioExecutor::new());
    sniffing.http2().enable_connect_protocol();
    let http1 = hyper::server::conn::http1::Builder::new();

    let (stop, _) = watch::channel(false);
    let mut conns: JoinSet<()> = JoinSet::new();
    let mut shutdown = pin!(shutdown);

    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(conn) => conn,
                // Usually the file descriptor limit. Pause briefly rather than spin on it.
                Err(err) => {
                    tracing::warn!("failed to accept a connection: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };

        let app = app.clone();
        let service = hyper::service::service_fn(move |mut req: Request<Incoming>| {
            req.extensions_mut().insert(ConnectInfo(peer));
            app.clone().call(req)
        });
        let io = TokioIo::new(stream);
        let mut stopping = stop.subscribe();

        if http2 {
            let conn = sniffing
                .serve_connection_with_upgrades(io, service)
                .into_owned();
            conns.spawn(async move {
                let mut conn = pin!(conn);
                let result = tokio::select! {
                    result = conn.as_mut() => result,
                    _ = stopping.changed() => {
                        conn.as_mut().graceful_shutdown();
                        conn.as_mut().await
                    }
                };
                if let Err(err) = result {
                    tracing::debug!(%peer, error = %err, "connection failed");
                }
            });
        } else {
            let conn = http1.serve_connection(io, service).with_upgrades();
            conns.spawn(async move {
                let mut conn = pin!(conn);
                let result = tokio::select! {
                    result = conn.as_mut() => result,
                    _ = stopping.changed() => {
                        conn.as_mut().graceful_shutdown();
                        conn.as_mut().await
                    }
                };
                if let Err(err) = result {
                    tracing::debug!(%peer, error = %err, "connection failed");
                }
            });
        }
    }

    // Stop taking new connections, ask the open ones to wind down, then wait them out.
    drop(listener);
    let _ = stop.send(true);
    while conns.join_next().await.is_some() {}
}
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c  => tracing::info!("received Ctrl-C, shutting down"),
        () = sigterm => tracing::info!("received SIGTERM, shutting down"),
    }
}
