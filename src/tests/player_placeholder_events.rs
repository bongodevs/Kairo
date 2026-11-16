use std::sync::Arc;
use std::time::Duration;

use player::buffer::BufferRead;
use player::decode::Millis;
use player::playback::PlayerConfig;
use player::source::{
    LoadFuture, OpenFuture, Source, SourceMetadata, SourceReference, SourceResult, Sources,
    TrackSource,
};
use player::track::codec::TrackCodecs;
use player::track::{SourceData, Track, TrackInfo};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use super::GuildId;
use crate::plugins::codec::PluginCodec;
use crate::session::{SessionSink, SessionStore, UpdateQueue};

/// A source that hands out generated silence and always fails its retry.
struct SilenceSource;

impl Source for SilenceSource {
    fn name(&self) -> &str {
        "silence"
    }

    fn claims(&self, identifier: &str) -> bool {
        identifier.starts_with("silence:")
    }

    fn load<'a>(&'a self, _: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async { Ok(SourceResult::NotMine) })
    }
}

impl TrackSource for SilenceSource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        let reference = track.reference().clone();
        let metadata = track.info().map_or_else(SourceMetadata::default, SourceMetadata::from);
        let retrying = player::source::fallback_suppressed();
        Box::pin(async move {
            if retrying {
                return Err(player::source::SourceError::load_failed(
                    "still down",
                    player::ExceptionSeverity::Common,
                ));
            }
            Ok(Some(crate::plugins::generated_silence("silence", &reference, &metadata)))
        })
    }
}

fn track() -> Track {
    let info = TrackInfo {
        identifier: String::from("silence:1"),
        is_seekable: true,
        author: String::from("Artist"),
        length: Some(Millis::new(30_000)),
        is_stream: false,
        title: String::from("Track"),
        uri: Some(String::from("silence:1")),
        source_name: String::from("silence"),
        artwork_url: None,
        isrc: None,
    };
    Track::from_parts(SourceReference::new("silence:1"), info, SourceData::None, Millis::default())
}

fn codecs() -> Arc<TrackCodecs> {
    let mut codecs = TrackCodecs::new();
    codecs.register(PluginCodec::new("silence")).expect("source codec");
    Arc::new(codecs)
}

fn store_with_sink() -> (Arc<SessionStore>, mpsc::Receiver<String>) {
    let store = Arc::new(SessionStore::default());
    assert!(store.reserve("session"));
    let (messages, receiver) = mpsc::channel(16);
    let sink = SessionSink {
        messages,
        updates: Arc::new(UpdateQueue::default()),
        cancel: watch::channel(None).0,
    };
    assert!(store.set_outbound("session", sink));
    (store, receiver)
}

#[tokio::test]
async fn the_end_of_generated_silence_is_one_common_exception() {
    let (store, mut messages) = store_with_sink();
    let mut sources = Sources::new();
    sources.register(SilenceSource).expect("silence source");
    let player = store
        .create_player("session", GuildId::new(42), sources, codecs(), PlayerConfig::default())
        .expect("create player")
        .expect("session");
    let mut frames = player.take_frame_consumer().expect("frame consumer");

    let silence = track();
    player.player().set_end_time(Some(Millis::new(100))).await.expect("end time");
    player.player().play(silence.make_clone(), false).await.expect("play");
    player.mark_placeholder(&silence);
    super::resume::start(&player);

    let mut packet = [0_u8; player::audio::MAX_PACKET_BYTES];
    let drain = async {
        while let BufferRead::Frame(_) | BufferRead::Empty =
            frames.read(&mut packet).expect("frame read")
        {
            tokio::task::yield_now().await;
        }
    };
    timeout(Duration::from_secs(3), drain).await.expect("the silence ended");

    let mut events = Vec::new();
    while let Ok(Some(message)) = timeout(Duration::from_millis(500), messages.recv()).await {
        events.push(serde_json::from_str::<serde_json::Value>(&message).expect("event JSON"));
    }
    player.shutdown().await.expect("shutdown");

    let starts = events.iter().filter(|event| event["type"] == "TrackStartEvent").count();
    let exceptions: Vec<_> =
        events.iter().filter(|event| event["type"] == "TrackExceptionEvent").collect();
    assert_eq!(starts, 1);
    assert_eq!(exceptions.len(), 1);
    assert_eq!(exceptions[0]["exception"]["severity"], "common");
    assert_eq!(exceptions[0]["exception"]["message"], "No source could play this track.");
    assert!(!events.iter().any(|event| event["type"] == "TrackEndEvent"));
    assert!(!events.iter().any(|event| event["type"] == "TrackStuckEvent"));
}
