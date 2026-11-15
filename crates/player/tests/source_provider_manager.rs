use kairo_audio::decode::Millis;
use kairo_core::track::codec::{DecodeTrack, EmptySourceCodec, TrackCodecError};
use kairo_core::track::{SourceData, Track, TrackInfo};

use super::*;
use crate::source::{
    LoadFuture, OpenFuture, Source, SourceConfig, SourceReference, SourceResult, TrackSource,
};
struct NamedSource(&'static str);

impl Source for NamedSource {
    fn name(&self) -> &str {
        self.0
    }

    fn load<'a>(&'a self, _reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async { Ok(SourceResult::NotMine) })
    }
}

impl TrackSource for NamedSource {
    fn open<'a>(&'a self, _track: &'a Track) -> OpenFuture<'a> {
        Box::pin(async { Ok(None) })
    }
}

fn track(source_name: &str) -> Track {
    Track::from_parts(
        SourceReference::new("id"),
        TrackInfo {
            identifier: String::from("id"),
            is_seekable: true,
            author: String::from("Artist"),
            length: Some(Millis::new(1_000)),
            is_stream: false,
            title: String::from("Title"),
            uri: Some(String::from("provider:id")),
            source_name: source_name.to_owned(),
            artwork_url: None,
            isrc: None,
        },
        SourceData::None,
        Millis::default(),
    )
}

#[test]
fn providers_install_the_source_and_matching_track_codec_together() {
    let mut providers = ProviderManager::new();
    providers.register(NamedSource("demo"), EmptySourceCodec::new("demo")).expect("provider");
    let sources = providers.build(SourceConfig { local: false, http: false }).expect("sources");
    assert_eq!(sources.names().collect::<Vec<_>>(), ["demo"]);

    let track = track("demo");
    let encoded = sources.encode(&track.snapshot(Millis::default())).expect("encode");
    let DecodeTrack::Track(decoded) = sources.decode_track(&encoded).expect("decode") else {
        panic!("expected track");
    };
    assert_eq!(decoded.info().map(|info| info.source_name.as_str()), Some("demo"));
}

#[test]
fn mismatched_or_duplicate_provider_names_are_rejected() {
    let mut providers = ProviderManager::new();
    let mismatch =
        providers.register(NamedSource("one"), EmptySourceCodec::new("two")).expect_err("mismatch");
    assert!(matches!(mismatch, ProviderError::NameMismatch { .. }));

    providers.register(NamedSource("demo"), EmptySourceCodec::new("demo")).expect("provider");
    let duplicate = providers
        .register(NamedSource("demo"), EmptySourceCodec::new("demo"))
        .expect_err("duplicate");
    assert!(matches!(duplicate, ProviderError::Codec(TrackCodecError::DuplicateSource { .. })));
}
