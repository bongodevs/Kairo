//! Loopback HTTP server and load/open assertions for provider tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use kairo_core::track::Track;

use crate::source::{LoadOutcome, OpenedSource, SourceReference, Sources};

/// One HTTP response served by [`Server`].
#[derive(Debug, Clone)]
pub struct Reply {
    status: u16,
    content_type: String,
    content_length: Option<usize>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    /// A `200` response with a content type and body.
    #[must_use]
    pub fn ok(content_type: impl Into<String>, body: impl AsRef<[u8]>) -> Self {
        Self {
            status: 200,
            content_type: content_type.into(),
            content_length: None,
            headers: Vec::new(),
            body: body.as_ref().to_vec(),
        }
    }

    /// A `200` JSON response.
    #[must_use]
    pub fn json(body: &str) -> Self {
        Self::ok("application/json", body)
    }

    /// A response with a status and no body.
    #[must_use]
    pub fn status(status: u16) -> Self {
        Self {
            status,
            content_type: String::new(),
            content_length: Some(0),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Add one response header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A loopback HTTP server that records every request it accepts.
pub struct Server {
    base: Url,
    requests: Arc<Mutex<Vec<String>>>,
    task: Option<JoinHandle<()>>,
}

impl Server {
    /// Serve one reply per request, in order, then answer `404`.
    pub async fn start(replies: Vec<Reply>) -> Self {
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        Self::serve(move |_, _| {
            replies.lock().expect("replies").pop_front().unwrap_or_else(|| Reply::status(404))
        })
        .await
    }

    /// Serve until the server is finished or dropped.
    pub async fn serve<F>(handler: F) -> Self
    where
        F: Fn(&str, &Url) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback server");
        let address = listener.local_addr().expect("loopback address");
        let base = Url::parse(&format!("http://{address}/")).expect("loopback url");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let handler = Arc::new(handler);
        let handler_base = base.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut socket).await;
                let reply = handler(&request, &handler_base);
                seen.lock().expect("requests").push(request);
                write_reply(&mut socket, reply).await;
            }
        });
        Self { base, requests, task: Some(task) }
    }

    /// The server root URL.
    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// A URL on this server.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        self.base.join(path).expect("server path").to_string()
    }

    /// The requests accepted so far.
    #[must_use]
    pub fn seen(&self) -> Vec<String> {
        self.requests.lock().expect("requests").clone()
    }

    /// Stop the server and return every request it accepted.
    pub async fn finish(mut self) -> Vec<String> {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        self.seen()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The request target from the first request line.
#[must_use]
pub fn request_path(request: &str) -> &str {
    request.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("/")
}

/// Load one identifier, panicking if the source reports an error.
pub async fn assert_load(sources: &Sources, identifier: &str) -> LoadOutcome {
    match sources.load(SourceReference::new(identifier)).await {
        LoadOutcome::Error(error) => panic!("load {identifier:?} failed: {error:?}"),
        outcome => outcome,
    }
}

/// Open one track, panicking when the source has no playback for it.
pub async fn assert_open(sources: &Sources, track: &Track) -> OpenedSource {
    match sources.open_track(track).await {
        Ok(Some(opened)) => opened,
        Ok(None) => panic!("no playback for {:?}", track.reference().identifier()),
        Err(error) => panic!("open failed: {error}"),
    }
}

/// Extract a loaded track.
pub fn assert_track(outcome: LoadOutcome) -> Track {
    match outcome {
        LoadOutcome::Track(track) => track,
        other => panic!("expected a track, got {other:?}"),
    }
}

/// Extract a search result.
pub fn assert_search(outcome: LoadOutcome) -> Vec<Track> {
    match outcome {
        LoadOutcome::Search(tracks) => tracks,
        other => panic!("expected a search, got {other:?}"),
    }
}

async fn read_request(socket: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => data.extend_from_slice(&buffer[..read]),
        }
        if let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&data[..header_end]).to_ascii_lowercase();
            if data.len().saturating_sub(header_end + 4) >= content_length(&header) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn content_length(header: &str) -> usize {
    header
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

async fn write_reply(socket: &mut TcpStream, reply: Reply) {
    let reason = reqwest::StatusCode::from_u16(reply.status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Error");
    let length = reply.content_length.unwrap_or(reply.body.len());
    let mut head = format!("HTTP/1.1 {} {reason}\r\nContent-Length: {length}\r\n", reply.status);
    if !reply.content_type.is_empty() {
        head.push_str(&format!("Content-Type: {}\r\n", reply.content_type));
    }
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    socket.write_all(head.as_bytes()).await.expect("response head");
    socket.write_all(&reply.body).await.expect("response body");
    socket.shutdown().await.expect("shutdown response");
}
