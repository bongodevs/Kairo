use std::io::Cursor;

use super::test_support::{OpusPlayer, next_frame, opus_packets};
use super::*;
use crate::audio::MAX_PACKET_BYTES;
use crate::buffer::BufferRead;
use crate::decode::Millis;
use crate::filters::Filters;
use crate::source::{LocalSource, OpenedSource, SourceHints, SourceReference, Sources};
use crate::track::Track;

async fn opened(fixture: &OpusPlayer) -> OpenedSource {
    let mut sources = Sources::new();
    sources.register(LocalSource).expect("register local");
    sources.open_track(&Track::from(fixture.track.clone())).await.expect("open").expect("source")
}

async fn play_to_position(fixture: &mut OpusPlayer, frames: usize) -> Millis {
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    assert!(matches!(fixture.events.recv().await, Some(PlayerEvent::TrackStart { .. })));
    for _ in 0..frames {
        let _ = next_frame(&mut fixture.frames).await;
    }
    fixture.player.state().await.expect("state").position
}

fn drain_events(events: &mut PlayerEvents) {
    while events.try_recv().is_ok() {}
}

#[tokio::test]
async fn replacing_media_keeps_the_track_position_volume_and_filters() {
    let mut fixture = OpusPlayer::new("replace-live", 300).await;
    let replacement = OpusPlayer::new("replace-live-source", 300).await;
    let playback = opened(&replacement).await;
    let _ = play_to_position(&mut fixture, 25).await;

    fixture.player.set_volume(30).await.expect("volume");
    let filters = Filters { volume: 0.5, ..Filters::default() };
    fixture.player.set_filters(filters).await.expect("filters");
    let before = fixture.player.state().await.expect("state");
    assert!(before.position >= Millis::new(400), "position advanced: {:?}", before.position);

    assert!(fixture.player.replace_media(playback, before.position).await.expect("replace"));

    let after = fixture.player.state().await.expect("state");
    assert_eq!(after.track, before.track);
    assert!(after.position >= before.position);
    assert_eq!(after.volume, 30);
    assert_eq!(after.filters, filters);
    assert!(fixture.events.try_recv().is_err(), "replacing media must not emit an event");

    let (frame, _) = next_frame(&mut fixture.frames).await;
    assert!(frame.position >= before.position, "new media starts at the reported position");

    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
    replacement.shutdown().await;
}

#[tokio::test]
async fn replacing_media_while_paused_stays_paused() {
    let mut fixture = OpusPlayer::new("replace-paused", 300).await;
    let replacement = OpusPlayer::new("replace-paused-source", 300).await;
    let playback = opened(&replacement).await;
    let _ = play_to_position(&mut fixture, 10).await;

    fixture.player.set_paused(true).await.expect("pause");
    drain_events(&mut fixture.events);
    let before = fixture.player.state().await.expect("state");

    assert!(fixture.player.replace_media(playback, before.position).await.expect("replace"));

    let after = fixture.player.state().await.expect("state");
    assert!(after.paused);
    assert_eq!(after.track, before.track);
    assert!(fixture.events.try_recv().is_err(), "replacing media must not emit an event");
    assert_eq!(
        fixture.frames.read(&mut [0; MAX_PACKET_BYTES]).expect("read while paused"),
        BufferRead::Empty
    );

    fixture.player.set_paused(false).await.expect("resume");
    let (frame, _) = next_frame(&mut fixture.frames).await;
    assert!(frame.position >= before.position);

    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
    replacement.shutdown().await;
}

#[tokio::test]
async fn replacing_media_without_a_track_is_a_clean_noop() {
    let replacement = OpusPlayer::new("replace-idle-source", 10).await;
    let playback = opened(&replacement).await;
    let (player, _frames, mut events, guard) =
        spawn_player(Sources::new(), PlayerConfig::default()).expect("spawn player");

    assert!(!player.replace_media(playback, Millis::new(500)).await.expect("replace"));
    assert!(events.try_recv().is_err());

    guard.shutdown().await.expect("shutdown");
    replacement.shutdown().await;
}

#[tokio::test]
async fn replacing_media_uses_the_new_stream_until_its_end() {
    let mut fixture = OpusPlayer::new("replace-short-live", 300).await;
    let replacement = OpusPlayer::new("replace-short-source", 50).await;
    let playback = opened(&replacement).await;
    let position = play_to_position(&mut fixture, 25).await;
    assert!(position >= Millis::new(400));

    assert!(fixture.player.replace_media(playback, position).await.expect("replace"));
    assert!(fixture.events.try_recv().is_err(), "replacing media must not emit an event");

    let mut output = [0; MAX_PACKET_BYTES];
    let mut frames = 0;
    loop {
        match fixture.frames.read(&mut output).expect("read") {
            BufferRead::Frame(_) => frames += 1,
            BufferRead::Empty => tokio::task::yield_now().await,
            BufferRead::End => break,
            BufferRead::Closed => panic!("closed before the replacement ended"),
        }
    }
    assert!((1..=40).contains(&frames), "short replacement played {frames} frames");
    assert!(matches!(
        fixture.events.recv().await,
        Some(PlayerEvent::TrackEnd { reason: TrackEndReason::Finished, .. })
    ));
    assert!(fixture.events.try_recv().is_err(), "the swap must not restart the track");

    fixture.shutdown().await;
    replacement.shutdown().await;
}

#[tokio::test]
async fn replacing_media_at_a_position_requires_a_seekable_stream() {
    let mut fixture = OpusPlayer::new("replace-unseekable-live", 300).await;
    let packets = opus_packets(50);
    let refs = packets.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let bytes = crate::test_ogg::opus_stream(&refs, 2, 0);
    let playback = OpenedSource::read_only(
        "memory",
        SourceReference::new("replace-unseekable"),
        SourceHints {
            mime_type: Some(String::from("audio/ogg")),
            file_extension: Some(String::from("ogg")),
        },
        Some(bytes.len() as u64),
        Cursor::new(bytes),
    );

    let position = play_to_position(&mut fixture, 25).await;
    assert!(position >= Millis::new(400));

    assert!(!fixture.player.replace_media(playback, position).await.expect("replace"));
    assert!(fixture.events.try_recv().is_err());

    let (frame, _) = next_frame(&mut fixture.frames).await;
    assert!(frame.position >= position, "the old stream keeps playing");

    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
