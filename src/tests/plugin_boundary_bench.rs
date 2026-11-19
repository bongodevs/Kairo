//! Ignored benchmark: the plugin boundary for `load` and `open` against a built-in source.
//!
//! Eight concurrent callers, 250 calls each, through the same `Sources` registry and probe the
//! server uses. The plugin is a local copy of `bongodevs/kairo-plugin-template`: `load` turns
//! `example:<url>` into a track without any I/O, and `open` returns that same URL, so the plugin
//! path pays for one host HTTP fetch that the built-in local path does not.
//!
//! ```text
//! # once: copy the template, let `resolve` accept http as well as https, then
//! cargo build --release --manifest-path <copy>/Cargo.toml
//! cp <copy>/target/release/libkairo_plugin_example.so plugins/example.kai
//!
//! cargo test --release --lib plugins::boundary_bench -- --ignored --nocapture
//! ```

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use player::source::{LoadOutcome, ProviderManager, SourceConfig, SourceReference, Sources};
use player::track::Track;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::config::PluginConfig;
use crate::plugins::PluginManager;
use crate::test_support::example_plugin;

const CLIENTS: usize = 8;
const CALLS_PER_CLIENT: usize = 250;
const PLUGIN: &str = "example";

type Call = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

struct Row {
    label: &'static str,
    p50: f64,
    p99: f64,
    wall: Duration,
    cpu: Duration,
    calls: usize,
}

impl Row {
    fn print(&self) {
        let throughput = self.calls as f64 / self.wall.as_secs_f64();
        let per_call = self.cpu.as_secs_f64() * 1e6 / self.calls as f64;
        println!(
            "| {} | {:.0} µs | {:.0} µs | {:.0}/s | {:.1} µs |",
            self.label, self.p50, self.p99, throughput, per_call
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark; needs plugins/example.kai from the plugin template"]
async fn plugin_and_builtin_load_and_open() {
    let plugin = example_plugin();
    assert!(plugin.is_file(), "install the template plugin first: {}", plugin.display());

    let root = std::env::temp_dir().join(format!("kairo-boundary-bench-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&root).await;
    tokio::fs::create_dir_all(&root).await.expect("bench root");
    let wav = root.join("bench.wav");
    tokio::fs::write(&wav, wav_bytes(48_000, 100)).await.expect("wav fixture");

    let body = tokio::fs::read(&wav).await.expect("wav bytes");
    let url = serve(body).await;
    let (manager, sources) = sources(&root, &plugin).await;

    let identifier = format!("example:http://{url}/bench.wav");
    let plugin_track = loaded(&sources, &identifier).await;
    let local_track = loaded(&sources, &wav.to_string_lossy()).await;
    let http_track = loaded(&sources, &format!("http://{url}/bench.wav")).await;

    println!("\n| path | p50 | p99 | throughput | CPU per call |");
    println!("|---|---|---|---|---|");
    row("load plugin", load(Arc::clone(&sources), identifier)).await.print();
    row("load built-in local", load(Arc::clone(&sources), wav.to_string_lossy().into_owned()))
        .await
        .print();
    row("open plugin", open(Arc::clone(&sources), plugin_track)).await.print();
    row("open built-in http (same URL)", open(Arc::clone(&sources), http_track)).await.print();
    row("open built-in local", open(Arc::clone(&sources), local_track)).await.print();

    drop(sources);
    manager.shutdown().await;
    let _ = tokio::fs::remove_dir_all(&root).await;
}

fn load(sources: Arc<Sources>, identifier: String) -> Call {
    Arc::new(move || {
        let sources = Arc::clone(&sources);
        let identifier = identifier.clone();
        async move {
            matches!(sources.load(SourceReference::new(identifier)).await, LoadOutcome::Track(_))
        }
        .boxed()
    })
}

fn open(sources: Arc<Sources>, track: Arc<Track>) -> Call {
    Arc::new(move || {
        let sources = Arc::clone(&sources);
        let track = Arc::clone(&track);
        async move { sources.open_track(&track).await.is_ok_and(|opened| opened.is_some()) }.boxed()
    })
}

async fn loaded(sources: &Sources, identifier: &str) -> Arc<Track> {
    match sources.load(SourceReference::new(identifier)).await {
        LoadOutcome::Track(track) => Arc::new(track),
        _ => panic!("{identifier} did not load a track"),
    }
}

async fn row(label: &'static str, call: Call) -> Row {
    for _ in 0..16 {
        assert!(call().await, "{label} failed during warmup");
    }
    let cpu = cpu_time();
    let started = Instant::now();
    let mut workers = JoinSet::new();
    for _ in 0..CLIENTS {
        let call = Arc::clone(&call);
        workers.spawn(async move {
            let mut samples = Vec::with_capacity(CALLS_PER_CLIENT);
            for _ in 0..CALLS_PER_CLIENT {
                let started = Instant::now();
                assert!(call().await, "{label} failed under load");
                samples.push(started.elapsed());
            }
            samples
        });
    }
    let mut samples = Vec::new();
    while let Some(worker) = workers.join_next().await {
        samples.extend(worker.expect("bench worker"));
    }
    let wall = started.elapsed();
    let cpu = cpu_time().saturating_sub(cpu);
    samples.sort_unstable();
    let at = |fraction: f64| {
        let index = ((samples.len() - 1) as f64 * fraction).round() as usize;
        samples[index].as_secs_f64() * 1e6
    };
    Row { label, p50: at(0.50), p99: at(0.99), wall, cpu, calls: samples.len() }
}

async fn sources(root: &Path, plugin: &Path) -> (PluginManager, Arc<Sources>) {
    let mut config =
        PluginConfig { name: PLUGIN.into(), repo: None, tag: None, config: Default::default() };
    config.config.insert(String::from("enabled"), toml::Value::Boolean(true));
    tokio::fs::copy(plugin, config.install_path(root)).await.expect("install template plugin");

    let manager = PluginManager::load(&[config], root).await.expect("plugin manager");
    let mut providers = ProviderManager::new().with_probe(player::source_probe::audio_probe());
    let mirror = manager
        .register_sources(&mut providers, ["http", "local"], Vec::new())
        .expect("register plugin sources");
    let sources =
        Arc::new(providers.build(SourceConfig { http: true, local: true }).expect("sources"));
    mirror.install(&sources);
    (manager, sources)
}

/// Answers every request with the same body; the plugin and the HTTP source fetch the same bytes.
async fn serve(body: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind bench server");
    let address = listener.local_addr().expect("bench address");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: audio/wav\r\n\
                     Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    address
}

/// A valid PCM WAV so the local probe measures the same metadata path the server uses.
fn wav_bytes(rate: u32, millis: u32) -> Vec<u8> {
    let data = rate * millis / 1000 * 2;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.resize(44 + data as usize, 0);
    bytes
}

/// User plus system CPU time of this process.
fn cpu_time() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let Some((_, fields)) = stat.rsplit_once(')') else {
        return Duration::ZERO;
    };
    let ticks: u64 = fields
        .split_whitespace()
        .skip(11)
        .take(2)
        .filter_map(|field| field.parse::<u64>().ok())
        .sum();
    Duration::from_millis(ticks * 10)
}
