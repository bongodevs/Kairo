use std::collections::HashMap;
use std::io::Cursor;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use player::decode::Millis;
use player::source::{
    LoadFuture, OpenFuture, OpenedSource, Source, SourceHints, SourceReference, SourceResult,
    Sources, TrackSource,
};
use player::track::codec::{self, TrackCodecs};
use player::track::{Playlist, SourceData, Track, TrackInfo};

use crate::app::{AppState, CpuSampler, router};
use crate::players::FilterPolicy;
use crate::session::SessionStore;

/// Collects formatted log lines in memory so a test can assert on them.
#[derive(Clone, Default)]
pub(crate) struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log capture")).into_owned()
    }
}

pub(crate) struct LogWriter(LogCapture);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.0.lock().expect("log capture").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.clone())
    }
}

pub(crate) fn capture_subscriber(capture: &LogCapture) -> impl tracing::Subscriber + Send + Sync {
    tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .finish()
}

pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: String,
}

impl HttpResponse {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }
}

struct MemorySource;

impl Source for MemorySource {
    fn name(&self) -> &str {
        "local"
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        let reference = reference.clone();
        Box::pin(async move {
            if reference.identifier() == "playlist" {
                return Ok(SourceResult::Playlist(Playlist::new("playlist", Vec::new())));
            }
            if reference.identifier() == "search" {
                return Ok(SourceResult::Playlist(Playlist::search("search", Vec::new())));
            }
            let (bytes, seekable) = match reference.identifier() {
                "memory.wav" => (test_wave(), true),
                "forward.wav" => (test_wave(), false),
                "long.wav" => (test_wave_seconds(10), true),
                _ => return Ok(SourceResult::NotMine),
            };
            let hints = SourceHints {
                mime_type: Some(String::from("audio/wav")),
                file_extension: Some(String::from("wav")),
            };
            let length = Some(bytes.len() as u64);
            let stream = Cursor::new(bytes);
            let opened = if seekable {
                OpenedSource::seekable("local", reference, hints, length, stream)
            } else {
                OpenedSource::read_only("local", reference, hints, length, stream)
            };
            Ok(SourceResult::Opened(opened))
        })
    }
}

impl TrackSource for MemorySource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        Box::pin(async move {
            match self.load(track.reference()).await? {
                SourceResult::Opened(opened) => Ok(Some(opened)),
                SourceResult::NotMine | SourceResult::Empty => Ok(None),
                _ => Err(player::source::SourceError::InvalidPlaybackResult {
                    name: String::from("local"),
                }),
            }
        })
    }
}

pub(crate) fn memory_sources() -> Sources {
    let mut sources = Sources::new();
    sources.register(MemorySource).expect("memory source");
    sources
}

/// The built `sources` cdylib that plugin tests copy to the install path.
pub(crate) fn sources_plugin() -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let installed = root.join("plugins").join(format!("sources.{}", kairo_plugin_api::EXTENSION));
    if installed.exists() {
        return installed;
    }
    root.join("crates/kairo-sources-plugin/target/debug").join(format!(
        "{}kairo_plugin_sources{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

/// A local copy of the plugin template (`bongodevs/kairo-plugin-template`), installed by hand.
pub(crate) fn example_plugin() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/example.kai")
}

pub(crate) fn encoded_track() -> String {
    let track = Track::from_parts(
        SourceReference::new("memory.wav"),
        TrackInfo {
            identifier: String::from("memory.wav"),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(1_000)),
            is_stream: false,
            title: String::from("Track"),
            uri: Some(String::from("memory.wav")),
            source_name: String::from("local"),
            artwork_url: None,
            isrc: None,
        },
        SourceData::probe("wav", None),
        Millis::default(),
    );
    codec::encode(&track.snapshot(Millis::default()), &TrackCodecs::new()).expect("encode track")
}

/// The same track shape as [`encoded_track`], but backed by ten seconds of audio so a test can
/// drive a transition while the track is still playing. `title` distinguishes two such tracks in
/// one queue.
pub(crate) fn long_encoded_track(title: &str) -> String {
    let track = Track::from_parts(
        SourceReference::new("long.wav"),
        TrackInfo {
            identifier: String::from("long.wav"),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(10_000)),
            is_stream: false,
            title: title.to_owned(),
            uri: Some(String::from("long.wav")),
            source_name: String::from("local"),
            artwork_url: None,
            isrc: None,
        },
        SourceData::probe("wav", None),
        Millis::default(),
    );
    codec::encode(&track.snapshot(Millis::default()), &TrackCodecs::new()).expect("encode track")
}

pub(crate) fn app_state(password: Option<&str>) -> AppState {
    app_state_with_sources(password, Sources::new())
}

pub(crate) fn app_state_with_sources(password: Option<&str>, sources: Sources) -> AppState {
    app_state_with_source_state(password, sources)
}

pub(crate) fn app_state_with_source_state(password: Option<&str>, sources: Sources) -> AppState {
    let sources = sources.with_probe(player::source_probe::audio_probe());
    app_state_with_shared_sources(password, Arc::new(sources))
}

pub(crate) fn app_state_with_shared_sources(
    password: Option<&str>,
    sources: Arc<Sources>,
) -> AppState {
    let codecs = sources.codecs_arc();
    AppState {
        password: password.map(str::to_owned),
        start: Instant::now(),
        cores: 1,
        cpu: Arc::new(CpuSampler { load: Mutex::new(None) }),
        sources,
        codecs,
        filter_policy: FilterPolicy::default(),
        player_config: player::playback::PlayerConfig::default(),
        player_update_interval: Duration::from_secs(5),
        sessions: Arc::new(SessionStore::default()),
        route_planner: None,
        metrics_endpoint: None,
        http2_enabled: false,
        search_metrics: Arc::new(crate::stats::SearchMetrics::default()),
        request_log: crate::config::RequestLoggingConfig::default(),
        shutdown: tokio::sync::watch::channel(false).0,
        plugins: Arc::new(crate::plugins::PluginManager::empty()),
        placeholders: Arc::new(crate::plugins::PlaceholderLog::default()),
    }
}

pub(crate) async fn spawn_server(state: AppState) -> SocketAddr {
    let app = router(state);
    let listener =
        TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).await.expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    tokio::spawn(async move {
        let _ =
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
    });
    address
}

pub(crate) async fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    body: Option<&str>,
) -> HttpResponse {
    request_with_headers(address, method, path, authorization, body, &[]).await
}

pub(crate) async fn request_with_headers(
    address: SocketAddr,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> HttpResponse {
    let body = body.unwrap_or_default();
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n");
    if let Some(authorization) = authorization {
        request.push_str(&format!("Authorization: {authorization}\r\n"));
    }
    if !body.is_empty()
        && !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        request.push_str("Content-Type: application/json\r\n");
    }
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));

    let mut stream = TcpStream::connect(address).await.expect("connect test server");
    stream.write_all(request.as_bytes()).await.expect("write test request");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read test response");
    parse_response(&response)
}

fn parse_response(response: &str) -> HttpResponse {
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((response, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap_or_default();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    HttpResponse { status, headers, body: body.to_owned() }
}

pub(crate) fn test_wave() -> Vec<u8> {
    test_wave_frames(2_400)
}

/// Silence long enough to outlast a test's command round trips at 48 kHz stereo.
pub(crate) fn test_wave_seconds(seconds: u32) -> Vec<u8> {
    test_wave_frames(48_000 * seconds)
}

pub(crate) fn test_wave_frames(frames: u32) -> Vec<u8> {
    let data_length = frames * 4;
    let mut bytes = Vec::with_capacity((44 + data_length) as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_length).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&48_000_u32.to_le_bytes());
    bytes.extend_from_slice(&192_000_u32.to_le_bytes());
    bytes.extend_from_slice(&4_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_length.to_le_bytes());
    bytes.resize((44 + data_length) as usize, 0);
    bytes
}
