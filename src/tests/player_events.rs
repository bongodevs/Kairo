use player::decode::Millis;
use player::playback::TrackException;
use player::source::SourceReference;
use player::track::codec::TrackCodecs;
use player::track::{SourceData, Track, TrackBuilder, TrackInfo};

use super::*;
use crate::players::GuildId;
fn track() -> player::track::TrackSnapshot {
    Track::from_parts(
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
    )
    .snapshot(Millis::new(20))
}

fn json(event: PlayerEvent) -> serde_json::Value {
    serde_json::from_str(
        &event_message(&event, GuildId::new(42), &TrackCodecs::new()).expect("event message"),
    )
    .expect("event JSON")
}

#[test]
fn source_metadata_survives_track_events_and_the_encoded_payload() {
    let plugin_info = serde_json::Map::from_iter([
        (String::from("save_uri"), serde_json::json!("https://www.jiosaavn.com/song/x/song-id")),
        (String::from("albumName"), serde_json::json!("Album")),
        (String::from("albumUrl"), serde_json::Value::Null),
        (String::from("artistUrl"), serde_json::Value::Null),
        (String::from("artistArtworkUrl"), serde_json::Value::Null),
        (String::from("previewUrl"), serde_json::Value::Null),
        (String::from("isPreview"), serde_json::json!(false)),
        (String::from("isLocal"), serde_json::json!(false)),
    ]);
    let track = TrackBuilder::new(
        SourceReference::new("song-id"),
        TrackInfo {
            identifier: String::from("song-id"),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(1_000)),
            is_stream: false,
            title: String::from("Song"),
            uri: Some(String::from("https://www.jiosaavn.com/song/x/song-id")),
            source_name: String::from("jiosaavn"),
            artwork_url: None,
            isrc: None,
        },
    )
    .plugin_info(plugin_info)
    .build();
    let snapshot = track.snapshot(Millis::default());
    let mut codecs = TrackCodecs::new();
    codecs.register(crate::plugins::codec::PluginCodec::new("jiosaavn")).expect("codec");
    let message = event_message(
        &PlayerEvent::TrackStart { track: snapshot.clone() },
        GuildId::new(42),
        &codecs,
    )
    .expect("event");
    let value: serde_json::Value = serde_json::from_str(&message).expect("JSON");
    assert_eq!(value["track"]["pluginInfo"]["albumName"], "Album");

    let encoded = value["track"]["encoded"].as_str().expect("encoded");
    let player::track::codec::DecodeTrack::Track(decoded) =
        player::track::codec::decode(encoded, &codecs).expect("decode")
    else {
        panic!("expected track");
    };
    assert_eq!(decoded.plugin_info(), snapshot.plugin_info());
}

#[test]
fn pause_and_resume_events_do_not_reach_the_wire() {
    for event in [PlayerEvent::Paused { track: track() }, PlayerEvent::Resumed { track: track() }] {
        assert!(event_message(&event, GuildId::new(42), &TrackCodecs::new()).is_none());
    }
}

#[test]
fn end_reasons_keep_the_canonical_lower_camel_values() {
    for (reason, expected) in [
        (TrackEndReason::Finished, "finished"),
        (TrackEndReason::LoadFailed, "loadFailed"),
        (TrackEndReason::Stopped, "stopped"),
        (TrackEndReason::Replaced, "replaced"),
        (TrackEndReason::Cleanup, "cleanup"),
        (TrackEndReason::Crossfade, "crossfade"),
        (TrackEndReason::Gapless, "gapless"),
    ] {
        let value = json(PlayerEvent::TrackEnd { track: track(), reason });
        assert_eq!(value["op"], "event");
        assert_eq!(value["type"], "TrackEndEvent");
        assert_eq!(value["guildId"], "42");
        assert_eq!(value["reason"], expected);
        assert!(value["track"]["encoded"].is_string());
    }
}

#[test]
fn exception_and_stuck_events_keep_the_v4_payloads() {
    let exception = json(PlayerEvent::TrackException {
        track: track(),
        exception: TrackException {
            message: Some(String::from("decoder failed")),
            severity: ExceptionSeverity::Suspicious,
            cause: String::from("bad packet"),
            cause_stack_trace: String::from("trace"),
        },
    });
    assert_eq!(exception["type"], "TrackExceptionEvent");
    assert_eq!(exception["exception"]["message"], "decoder failed");
    assert_eq!(exception["exception"]["severity"], "suspicious");
    assert_eq!(exception["exception"]["cause"], "bad packet");
    assert_eq!(exception["exception"]["causeStackTrace"], "trace");

    let stuck = json(PlayerEvent::TrackStuck { track: track(), threshold_ms: 10_000 });
    assert_eq!(stuck["type"], "TrackStuckEvent");
    assert_eq!(stuck["thresholdMs"], 10_000);
}

#[test]
fn lifecycle_logs_name_the_session_guild_and_track_but_not_the_payload() {
    let capture = crate::test_support::LogCapture::default();
    let _guard =
        tracing::subscriber::set_default(crate::test_support::capture_subscriber(&capture));

    let track = track();
    let guild = GuildId::new(42);
    log_event("session", guild, &PlayerEvent::TrackStart { track: track.clone() });
    log_event(
        "session",
        guild,
        &PlayerEvent::TrackEnd { track: track.clone(), reason: TrackEndReason::Finished },
    );
    log_event(
        "session",
        guild,
        &PlayerEvent::TrackStuck { track: track.clone(), threshold_ms: 10_000 },
    );
    log_event(
        "session",
        guild,
        &PlayerEvent::TrackException {
            track,
            exception: TrackException {
                message: Some(String::from("decoder failed")),
                severity: ExceptionSeverity::Suspicious,
                cause: String::from("cause"),
                cause_stack_trace: String::from("trace"),
            },
        },
    );

    let text = capture.text();
    for line in ["track started", "track ended", "track stuck", "track exception"] {
        assert!(text.contains(line), "missing {line} in {text}");
    }
    assert!(text.contains("session_id=\"session\""), "{text}");
    assert!(text.contains("guild_id=42"), "{text}");
    assert!(text.contains("track=\"memory.wav\""), "{text}");
    assert!(text.contains("reason=\"finished\""), "{text}");
    assert!(text.contains("severity=\"suspicious\""), "{text}");
    assert!(!text.contains("encoded"), "{text}");
}

#[test]
fn placeholder_failure_is_one_canonical_common_exception() {
    let text = placeholder_exception(&track(), GuildId::new(42), &TrackCodecs::new())
        .expect("placeholder exception");
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");

    assert_eq!(value["op"], "event");
    assert_eq!(value["type"], "TrackExceptionEvent");
    assert_eq!(value["guildId"], "42");
    assert_eq!(value["exception"]["message"], PLACEHOLDER_MESSAGE);
    assert_eq!(value["exception"]["severity"], "common");
    assert_eq!(value["exception"]["cause"], PLACEHOLDER_MESSAGE);
    assert_eq!(value["exception"]["causeStackTrace"], "");
    assert!(value["track"]["encoded"].is_string());
}
