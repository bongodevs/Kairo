//! Resilience scenarios that cross module boundaries: the mirror chain with real sources, playback
//! that outlives its source, generated silence that seeks to the elapsed position, and a media URL
//! that expires over loopback HTTP.

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use player::ExceptionSeverity;
use player::buffer::BufferRead;
use player::decode::Millis;
use player::playback::{PlayerConfig, spawn_player};
use player::source::{
    HttpSource, LoadFuture, OpenFuture, OpenedSource, Source, SourceError, SourceHints,
    SourceMetadata, SourceReference, SourceResult, Sources, TrackSource,
};
use player::track::{SourceData, Track, TrackInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

use super::health::BUSY_MESSAGE;
use super::placeholder;
use crate::config::FallbackPlaceholder;

/// A provider that claims `wave:` references and serves one second of WAV.
struct WaveProvider;

impl Source for WaveProvider {
    fn name(&self) -> &str {
        "wave"
    }

    fn claims(&self, identifier: &str) -> bool {
        identifier.starts_with("wave:")
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        let identifier = reference.identifier();
        let info = TrackInfo {
            identifier: identifier.to_owned(),
            is_seekable: true,
            author: String::from("Provider Artist"),
            length: Some(Millis::new(1_000)),
            is_stream: false,
            title: String::from("Provider Title"),
            uri: Some(identifier.to_owned()),
            source_name: String::from("wave"),
            artwork_url: None,
            isrc: None,
        };
        let track = Track::with_info(SourceReference::new(identifier), info);
        Box::pin(async move { Ok(SourceResult::Track(track)) })
    }
}

impl TrackSource for WaveProvider {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        let reference = track.reference().clone();
        Box::pin(async move {
            let bytes = crate::test_support::test_wave();
            let length = Some(bytes.len() as u64);
            Ok(Some(OpenedSource::read_only(
                "wave",
                reference,
                SourceHints {
                    mime_type: Some(String::from("audio/wav")),
                    file_extension: Some(String::from("wav")),
                },
                length,
                Cursor::new(bytes),
            )))
        })
    }
}

struct BusySource;

impl Source for BusySource {
    fn name(&self) -> &str {
        "busy"
    }

    fn claims(&self, identifier: &str) -> bool {
        identifier.starts_with("busy:")
    }

    fn load<'a>(&'a self, _: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async { Err(SourceError::load_failed(BUSY_MESSAGE, ExceptionSeverity::Common)) })
    }
}

impl TrackSource for BusySource {
    fn open<'a>(&'a self, _: &'a Track) -> OpenFuture<'a> {
        Box::pin(async { Err(SourceError::load_failed(BUSY_MESSAGE, ExceptionSeverity::Common)) })
    }
}

/// A source whose playback fails after the track has been opened.
struct StreamSource {
    healthy: Arc<AtomicBool>,
}

impl Source for StreamSource {
    fn name(&self) -> &str {
        "stream"
    }

    fn claims(&self, identifier: &str) -> bool {
        identifier.starts_with("stream:")
    }

    fn load<'a>(&'a self, _: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async { Ok(SourceResult::NotMine) })
    }
}

impl TrackSource for StreamSource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        let healthy = self.healthy.load(Ordering::SeqCst);
        let reference = track.reference().clone();
        Box::pin(async move {
            if !healthy {
                return Err(SourceError::load_failed(
                    "the source is down",
                    ExceptionSeverity::Common,
                ));
            }
            let bytes = crate::test_support::test_wave_seconds(10);
            let length = Some(bytes.len() as u64);
            Ok(Some(OpenedSource::read_only(
                "stream",
                reference,
                SourceHints {
                    mime_type: Some(String::from("audio/wav")),
                    file_extension: Some(String::from("wav")),
                },
                length,
                Cursor::new(bytes),
            )))
        })
    }
}

fn track(identifier: &str, source: &str, length: u64, format: &str) -> Track {
    let info = TrackInfo {
        identifier: identifier.to_owned(),
        is_seekable: true,
        author: String::from("Artist"),
        length: Some(Millis::new(length)),
        is_stream: false,
        title: String::from("Track"),
        uri: Some(identifier.to_owned()),
        source_name: source.to_owned(),
        artwork_url: None,
        isrc: None,
    };
    Track::from_parts(
        SourceReference::new(identifier),
        info,
        SourceData::probe(format, None),
        Millis::default(),
    )
}

fn provider(source: &str, template: &str) -> crate::config::FallbackProvider {
    crate::config::FallbackProvider {
        source: source.to_owned(),
        template: template.to_owned(),
        placeholder: FallbackPlaceholder::Query,
    }
}

#[tokio::test]
async fn a_busy_source_is_skipped_and_the_provider_plays_with_the_outer_identity() {
    let mut sources = Sources::new();
    sources.register(BusySource).expect("busy source");
    sources.register(WaveProvider).expect("provider source");
    let sources = Arc::new(sources);
    let chain = super::mirror::MirrorChain::new(vec![
        provider("busy", "busy:%QUERY%"),
        provider("wave", "wave:%QUERY%"),
    ]);
    chain.install(&sources);

    let metadata = SourceMetadata {
        title: Some(String::from("Outer")),
        author: Some(String::from("Outer Artist")),
        ..SourceMetadata::default()
    };
    let mut opened = chain
        .open(
            "outer",
            SourceReference::new("outer:1"),
            metadata.clone(),
            &super::mirror::MirrorSpec {
                isrc: None,
                title: String::from("Outer"),
                author: String::from("Outer Artist"),
            },
        )
        .await
        .expect("provider opens");

    assert_eq!(opened.source_name(), "outer");
    assert_eq!(opened.reference().identifier(), "outer:1");
    assert_eq!(opened.metadata(), &metadata);
    let mut riff = [0; 4];
    opened.read(&mut riff).await.expect("provider bytes");
    assert_eq!(&riff, b"RIFF");
}

#[tokio::test]
async fn playback_continues_after_its_source_stops_opening() {
    let healthy = Arc::new(AtomicBool::new(true));
    let mut sources = Sources::new();
    sources.register(StreamSource { healthy: Arc::clone(&healthy) }).expect("streaming source");
    let sources = sources.with_probe(player::source_probe::audio_probe());
    let (player, mut frames, _events, guard) =
        spawn_player(sources.clone(), PlayerConfig::default()).expect("player");
    let stream = track("stream:long", "stream", 10_000, "wav");
    player.play(stream.make_clone(), false).await.expect("play");

    read_frames(&mut frames, 5).await;
    healthy.store(false, Ordering::SeqCst);
    assert!(sources.open_track(&stream).await.is_err(), "the source is now down");
    read_frames(&mut frames, 5).await;

    guard.shutdown().await.expect("shutdown");
}

async fn read_frames(frames: &mut player::buffer::FrameConsumer, count: usize) {
    let mut packet = [0_u8; player::audio::MAX_PACKET_BYTES];
    let read = async {
        for _ in 0..count {
            loop {
                match frames.read(&mut packet).expect("frame read") {
                    BufferRead::Frame(_) => break,
                    BufferRead::Empty => tokio::task::yield_now().await,
                    BufferRead::End | BufferRead::Closed => panic!("playback ended early"),
                }
            }
        }
    };
    timeout(Duration::from_secs(3), read).await.expect("frames kept arriving");
}

#[tokio::test]
async fn generated_silence_seeks_to_the_elapsed_position() {
    use player::decode::{AudioBlock, BlockingDecoder};
    use player::opus::OpusRoute;

    let reference = SourceReference::new("silence:1");
    let source = placeholder::opened_source("silence", &reference, &SourceMetadata::default());
    let runtime = tokio::runtime::Handle::current();
    let frames = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source, runtime).expect("open placeholder");
        assert!(decoder.seek(Millis::new(29_800)).expect("seek placeholder"));
        let mut frames = 0u64;
        while let Some(block) = decoder
            .next_audio(OpusRoute { volume: 100, filters_active: false })
            .expect("decode placeholder")
        {
            frames += match block {
                AudioBlock::Pcm(block) => u64::try_from(block.frames).expect("frames"),
                AudioBlock::Opus(block) => {
                    u64::try_from(block.info.total_samples).expect("samples")
                }
            };
        }
        frames
    })
    .await
    .expect("decode worker");

    // The last 200 ms of the stream, with a little slack for page granularity.
    assert!(frames > 0, "no audio after the seek");
    assert!(frames < 48_000 * 3, "the seek did not move the decoder forward");
}

#[tokio::test]
async fn an_expired_media_url_is_reopened_once_over_loopback_http() {
    let media = crate::test_support::test_wave_seconds(1);
    let split = 400;
    let (url, opens) = spawn_expiring_media(media.clone(), split).await;
    let mut sources = Sources::new();
    sources.register(HttpSource::new().expect("http source")).expect("register http");
    let mut opened = sources.open(SourceReference::new(url)).await.expect("open").expect("media");

    let mut first = vec![0; split];
    opened.read(&mut first).await.expect("first read");
    assert_eq!(first, media[..split]);

    let mut rest = vec![0; media.len() - split];
    opened.read(&mut rest).await.expect("re-resolved read");
    assert_eq!(rest, media[split..]);
    assert_eq!(opens.load(Ordering::SeqCst), 2);
}

/// Serve one truncated response, refuse the range reconnect, then serve the whole body again.
async fn spawn_expiring_media(media: Vec<u8>, split: usize) -> (String, Arc<AtomicUsize>) {
    let listener =
        TcpListener::bind((std::net::IpAddr::from([127, 0, 0, 1]), 0)).await.expect("listener");
    let address = listener.local_addr().expect("address");
    let opens = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&opens);
    let length = media.len();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut request = vec![0; 4096];
            let read = stream.read(&mut request).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            if request.contains("\r\nrange:") {
                let body = b"refused";
                let head = format!(
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body).await;
                continue;
            }
            let open = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let body = if open == 1 { &media[..split] } else { &media[..] };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body).await;
        }
    });
    (format!("http://{address}/media.wav"), opens)
}
