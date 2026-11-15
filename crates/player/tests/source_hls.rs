use std::time::Duration;

use kairo_core::track::SourceData;

use super::*;
use crate::source::test_support::{Reply, Server, assert_track, request_path};
use crate::source::{SourceReference, Sources};

#[tokio::test]
async fn hls_mpeg_ts_is_loaded_through_the_adts_decoder() {
    let server = Server::serve(|request, _| match request_path(request) {
        "/live.m3u8" => Reply::ok(
            "application/vnd.apple.mpegurl",
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nmedia.m3u8\n",
        ),
        "/media.m3u8" => Reply::ok(
            "application/vnd.apple.mpegurl",
            "#EXTM3U\n#EXTINF:1,segment\nsegment.ts\n#EXT-X-ENDLIST\n",
        ),
        "/segment.ts" | "/direct.ts" => Reply::ok("video/mp2t", crate::test_mpegts::mpeg_ts_adts()),
        _ => Reply::status(404),
    })
    .await;

    let mut sources = Sources::new().with_probe(crate::test_audio_probe::audio_probe());
    sources.register(HttpSource::new().expect("HTTP source")).expect("register");
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        sources.load(SourceReference::new(server.url("live.m3u8"))),
    )
    .await
    .expect("HLS load timeout");
    let track = assert_track(outcome);
    let info = track.info().expect("track info");
    assert!(info.is_stream);
    assert_eq!(info.author, crate::test_mpegts::service_author());
    assert_eq!(info.title, crate::test_mpegts::service_title());
    assert!(matches!(
        track.source_data(),
        SourceData::Probe { name, .. } if name == "aac"
    ));
    assert!(server.seen().len() >= 3);

    let direct = tokio::time::timeout(
        Duration::from_secs(3),
        sources.load(SourceReference::new(server.url("direct.ts"))),
    )
    .await
    .expect("MPEG-TS load timeout");
    let track = assert_track(direct);
    assert!(matches!(
        track.source_data(),
        SourceData::Probe { name, .. } if name == "aac"
    ));
    let info = track.info().expect("direct track info");
    assert_eq!(info.author, crate::test_mpegts::service_author());
    assert_eq!(info.title, crate::test_mpegts::service_title());

    let _ = server.finish().await;
}
