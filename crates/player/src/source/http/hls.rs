use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use tokio::io::{AsyncRead, ReadBuf};

use super::source::{HlsManifest, HlsSegment, parse_hls};
use super::stream::{Body, read_bounded, response_body};
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const SEGMENT_WAIT_STEP: Duration = Duration::from_millis(200);

type SegmentFuture = Pin<Box<dyn Future<Output = io::Result<Option<LoadedSegment>>> + Send + Sync>>;

struct LoadedSegment {
    playlist_url: Url,
    segment_url: Url,
    body: Body,
}

enum ReadState {
    Idle,
    Loading(SegmentFuture),
    Reading(Body),
    End,
}

pub(crate) struct HlsStream {
    client: Client,
    origin_url: Url,
    playlist_url: Option<Url>,
    initial_manifest: Option<String>,
    last_segment: Option<Url>,
    state: ReadState,
}

impl HlsStream {
    pub(crate) fn new(client: Client, origin_url: Url, initial_manifest: String) -> Self {
        Self {
            client,
            origin_url,
            playlist_url: None,
            initial_manifest: Some(initial_manifest),
            last_segment: None,
            state: ReadState::Idle,
        }
    }

    fn begin_load(&mut self) {
        let client = self.client.clone();
        let origin_url = self.origin_url.clone();
        let playlist_url = self.playlist_url.clone();
        let initial_manifest = self.initial_manifest.take();
        let last_segment = self.last_segment.clone();
        self.state = ReadState::Loading(Box::pin(load_segment(
            client,
            origin_url,
            playlist_url,
            initial_manifest,
            last_segment,
        )));
    }
}

impl AsyncRead for HlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let stream = self.get_mut();
        loop {
            match &mut stream.state {
                ReadState::Idle => stream.begin_load(),
                ReadState::Loading(future) => match ready!(future.as_mut().poll(context))? {
                    Some(loaded) => {
                        stream.playlist_url = Some(loaded.playlist_url);
                        stream.last_segment = Some(loaded.segment_url);
                        stream.state = ReadState::Reading(loaded.body);
                    }
                    None => stream.state = ReadState::End,
                },
                ReadState::Reading(body) => {
                    let before = output.filled().len();
                    ready!(body.as_mut().poll_read(context, output))?;
                    if output.filled().len() > before {
                        return Poll::Ready(Ok(()));
                    }
                    stream.state = ReadState::Idle;
                }
                ReadState::End => return Poll::Ready(Ok(())),
            }
        }
    }
}

async fn load_segment(
    client: Client,
    origin_url: Url,
    playlist_url: Option<Url>,
    initial_manifest: Option<String>,
    last_segment: Option<Url>,
) -> io::Result<Option<LoadedSegment>> {
    let (playlist_url, mut manifest) = match playlist_url {
        Some(url) => {
            let manifest = fetch_manifest(&client, &url).await?;
            (url, manifest)
        }
        None => resolve_media_playlist(&client, origin_url, initial_manifest).await?,
    };
    let started = Instant::now();

    loop {
        let HlsManifest::Media { segments, ended } =
            parse_hls(&manifest, &playlist_url).map_err(io::Error::other)?
        else {
            return Err(io::Error::other("nested HLS master playlists are not supported"));
        };
        if let Some(segment) = choose_segment(&segments, last_segment.as_ref()) {
            let response =
                client.get(segment.url.clone()).send().await.map_err(io::Error::other)?;
            let status = response.status().as_u16();
            if !matches!(status, 200 | 203 | 206) {
                return Err(io::Error::other(format!(
                    "HLS segment request returned status {status}"
                )));
            }
            return Ok(Some(LoadedSegment {
                playlist_url,
                segment_url: segment.url.clone(),
                body: response_body(response),
            }));
        }
        if ended || !should_wait(started, &segments) {
            return Ok(None);
        }
        tokio::time::sleep(SEGMENT_WAIT_STEP).await;
        manifest = fetch_manifest(&client, &playlist_url).await?;
    }
}

async fn resolve_media_playlist(
    client: &Client,
    mut url: Url,
    initial_manifest: Option<String>,
) -> io::Result<(Url, String)> {
    let mut manifest = match initial_manifest {
        Some(manifest) => manifest,
        None => fetch_manifest(client, &url).await?,
    };
    for _ in 0..5 {
        match parse_hls(&manifest, &url).map_err(io::Error::other)? {
            HlsManifest::Master(variant) => {
                url = variant;
                manifest = fetch_manifest(client, &url).await?;
            }
            HlsManifest::Media { .. } => return Ok((url, manifest)),
        }
    }
    Err(io::Error::other("HLS master redirect limit reached"))
}

async fn fetch_manifest(client: &Client, url: &Url) -> io::Result<String> {
    let response = client.get(url.clone()).send().await.map_err(io::Error::other)?;
    let status = response.status().as_u16();
    if !matches!(status, 200 | 203 | 206) {
        return Err(io::Error::other(format!("HLS manifest request returned status {status}")));
    }
    let bytes = read_bounded(Vec::new(), response_body(response), MAX_MANIFEST_BYTES).await?;
    String::from_utf8(bytes).map_err(|_| io::Error::other("HLS manifest is not UTF-8"))
}

fn choose_segment<'a>(
    segments: &'a [HlsSegment],
    last_segment: Option<&Url>,
) -> Option<&'a HlsSegment> {
    let Some(last_segment) = last_segment else {
        return segments.first();
    };
    match segments.iter().position(|segment| segment.url == *last_segment) {
        Some(index) => segments.get(index + 1),
        None => segments.first(),
    }
}

fn should_wait(started: Instant, segments: &[HlsSegment]) -> bool {
    segments
        .first()
        .and_then(|segment| segment.duration)
        .is_some_and(|duration| started.elapsed() < duration)
}

#[cfg(test)]
#[path = "../../../tests/source_hls_state.rs"]
mod tests;
