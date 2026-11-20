use std::sync::Arc;

use super::*;
use crate::track::codec::{DecodeTrack, OpaqueSourceCodec, TrackCodecError, TrackCodecs};
use crate::{Millis, SourceReference};
fn info(source_name: &str) -> TrackInfo {
    TrackInfo {
        identifier: String::from("identifier"),
        is_seekable: true,
        author: String::from("Artist"),
        length: Some(Millis::new(210_000)),
        is_stream: false,
        title: String::from("Song \0 🌋"),
        uri: Some(String::from("https://example.test/audio")),
        source_name: source_name.to_owned(),
        artwork_url: Some(String::from("https://example.test/image")),
        isrc: Some(String::from("US123")),
    }
}

fn track(source_name: &str, source_data: SourceData) -> Track {
    Track::from_parts(
        SourceReference::with_title("identifier", "Song \0 🌋"),
        info(source_name),
        source_data,
        Millis::new(12_345),
    )
}

#[test]
fn version_three_round_trips_probe_payload_and_start_position() {
    let codecs = TrackCodecs::new();
    let track = track("http", SourceData::probe("ogg", Some(String::from("opus"))));
    let encoded = codec::encode(&track.snapshot(Millis::new(54_321)), &codecs).expect("encode");
    let DecodeTrack::Track(decoded) = codec::decode(&encoded, &codecs).expect("decode") else {
        panic!("expected track");
    };
    assert_eq!(decoded.info(), track.info());
    // The live position the snapshot was taken at must not follow the track onto the wire.
    assert_eq!(decoded.initial_position(), Millis::new(12_345));
    assert_eq!(decoded.source_data(), track.source_data());
    assert_eq!(decoded.reference().identifier(), "identifier");
}

#[test]
fn encoded_identity_is_stable_as_the_live_position_advances() {
    let codecs = TrackCodecs::new();
    let track = track("http", SourceData::probe("ogg", None));
    let at_start = codec::encode(&track.snapshot(Millis::default()), &codecs).expect("encode");
    let mid_track = codec::encode(&track.snapshot(Millis::new(90_000)), &codecs).expect("encode");
    assert_eq!(at_start, mid_track);
}

#[test]
fn seeking_does_not_change_the_encoded_identity() {
    let codecs = TrackCodecs::new();
    let mut track = track("http", SourceData::probe("ogg", None));
    let before = codec::encode(&track.snapshot(Millis::default()), &codecs).expect("encode");
    track.set_initial_position(Millis::new(30_000));
    let after = codec::encode(&track.snapshot(Millis::new(30_000)), &codecs).expect("encode");
    assert_eq!(before, after);
    assert_eq!(track.initial_position(), Millis::new(30_000));
}

#[test]
fn unknown_sources_keep_their_payload_opaque() {
    let mut encoder_codecs = TrackCodecs::new();
    encoder_codecs.register(OpaqueSourceCodec::new("custom-source", 2)).expect("register");
    let track = track("custom-source", SourceData::Opaque(Arc::from([7_u8, 9_u8])));
    let encoded = codec::encode(&track.snapshot(Millis::new(4_000)), &encoder_codecs)
        .expect("encode custom source");
    let DecodeTrack::Track(decoded) =
        codec::decode(&encoded, &TrackCodecs::new()).expect("decode envelope")
    else {
        panic!("expected opaque track");
    };
    assert_eq!(decoded.info(), track.info());
    assert_eq!(decoded.source_data(), track.source_data());
    assert_eq!(decoded.initial_position(), track.initial_position());
    assert_eq!(
        codec::encode(&decoded.snapshot(Millis::new(4_000)), &TrackCodecs::new())
            .expect("re-encode opaque source"),
        encoded
    );
}

#[test]
fn matching_custom_source_preserves_opaque_payload() {
    let mut codecs = TrackCodecs::new();
    codecs.register(OpaqueSourceCodec::new("custom-source", 3)).expect("register");
    let track = track("custom-source", SourceData::Opaque(Arc::from([1_u8, 2_u8, 3_u8])));
    let encoded = codec::encode(&track.snapshot(Millis::default()), &codecs).expect("encode");
    let DecodeTrack::Track(decoded) = codec::decode(&encoded, &codecs).expect("decode") else {
        panic!("expected track");
    };
    assert_eq!(decoded.source_data(), track.source_data());
}

#[test]
fn duplicate_and_mismatched_source_codecs_are_rejected() {
    let mut codecs = TrackCodecs::new();
    assert!(matches!(
        codecs.register(OpaqueSourceCodec::new("http", 0)),
        Err(TrackCodecError::DuplicateSource { .. })
    ));
    let track = track("http", SourceData::None);
    assert!(matches!(
        codec::encode(&track.snapshot(Millis::default()), &codecs),
        Err(TrackCodecError::WrongSourceData)
    ));
}

#[test]
fn malformed_base64_is_rejected() {
    assert!(matches!(
        codec::decode("not-base64", &TrackCodecs::new()),
        Err(TrackCodecError::InvalidBase64)
    ));
}

#[test]
fn future_track_version_is_a_typed_non_error_outcome() {
    let codecs = TrackCodecs::new();
    let track = track("http", SourceData::probe("ogg", None));
    let encoded = codec::encode(&track.snapshot(Millis::default()), &codecs).expect("encode");
    let mut bytes = codec::decode_base64_for_test(&encoded).expect("base64");
    bytes[4] = 4;
    let encoded = codec::encode_base64_for_test(&bytes);
    assert!(matches!(
        codec::decode(&encoded, &codecs).expect("decode envelope"),
        DecodeTrack::UnsupportedVersion { version: 4 }
    ));
}

#[test]
fn base64_vectors_match_the_standard_alphabet() {
    for (plain, encoded) in [
        (b"".as_slice(), ""),
        (b"f".as_slice(), "Zg=="),
        (b"fo".as_slice(), "Zm8="),
        (b"foo".as_slice(), "Zm9v"),
        (b"hello".as_slice(), "aGVsbG8="),
    ] {
        assert_eq!(codec::encode_base64_for_test(plain), encoded);
        assert_eq!(codec::decode_base64_for_test(encoded).expect("decode"), plain);
    }
}

#[test]
fn envelope_header_carries_body_size_and_version_flag() {
    let codecs = TrackCodecs::new();
    let track = track("http", SourceData::probe("ogg", None));
    let encoded = codec::encode(&track.snapshot(Millis::default()), &codecs).expect("encode");
    let bytes = codec::decode_base64_for_test(&encoded).expect("base64");
    let header = u32::from_be_bytes(bytes[..4].try_into().expect("header"));
    assert_eq!(header >> 30, 1);
    assert_eq!((header & 0x3fff_ffff) as usize, bytes.len() - 4);
    assert_eq!(bytes[4], 3);
}

#[test]
fn version_one_without_nullable_fields_is_readable() {
    let mut body = vec![1];
    codec::write_text_for_test(&mut body, "title").expect("title");
    codec::write_text_for_test(&mut body, "author").expect("author");
    body.extend_from_slice(&5_i64.to_be_bytes());
    codec::write_text_for_test(&mut body, "id").expect("identifier");
    body.push(0);
    codec::write_text_for_test(&mut body, "kairo").expect("source");
    body.extend_from_slice(&9_i64.to_be_bytes());
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&((body.len() as u32) | (1 << 30)).to_be_bytes());
    bytes.extend_from_slice(&body);

    let DecodeTrack::Track(track) =
        codec::decode(&codec::encode_base64_for_test(&bytes), &TrackCodecs::new()).expect("decode")
    else {
        panic!("expected track");
    };
    let info = track.info().expect("info");
    assert_eq!(info.title, "title");
    assert_eq!(info.uri, None);
    assert_eq!(info.artwork_url, None);
    assert_eq!(track.initial_position(), Millis::new(9));
}

#[test]
fn truncated_envelope_is_rejected() {
    assert!(matches!(
        codec::decode("QAAAAA==", &TrackCodecs::new()),
        Err(TrackCodecError::Truncated)
    ));
}

#[test]
fn oversized_modified_utf_field_is_rejected() {
    let mut info = info("http");
    info.title = "x".repeat(70_000);
    let track = Track::from_parts(
        SourceReference::new("identifier"),
        info,
        SourceData::probe("ogg", None),
        Millis::default(),
    );
    assert!(matches!(
        codec::encode(&track.snapshot(Millis::default()), &TrackCodecs::new()),
        Err(TrackCodecError::FieldTooLong)
    ));
}

#[test]
fn mutation_test_track_codec_decoder_never_panics_or_hangs() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0
        }
        fn next_usize(&mut self, max: usize) -> usize {
            if max == 0 { 0 } else { (self.next() as usize) % max }
        }
    }

    let seed: u64 = 0xCAFE_BABE_1234_5678;
    let mut rng = Rng(seed);
    let codecs = TrackCodecs::new();

    // Valid base64 tracks of different shapes and versions
    let track1 = track("http", SourceData::probe("ogg", Some(String::from("opus"))));
    let encoded1 = codec::encode(&track1.snapshot(Millis::new(54_321)), &codecs).expect("encode");
    let track2 = track("http", SourceData::probe("ogg", None));
    let encoded2 = codec::encode(&track2.snapshot(Millis::default()), &codecs).expect("encode");

    let valid_seeds = [encoded1.into_bytes(), encoded2.into_bytes(), b"QAAAAA==".to_vec()];

    for iteration in 0..100_000 {
        let base = &valid_seeds[rng.next_usize(valid_seeds.len())];
        let mut mutated = base.clone();

        match rng.next() % 4 {
            0 => {
                // Flip random bytes
                let count = 1 + rng.next_usize(4);
                for _ in 0..count {
                    if !mutated.is_empty() {
                        let idx = rng.next_usize(mutated.len());
                        mutated[idx] ^= (rng.next() & 0xFF) as u8;
                    }
                }
            }
            1 => {
                // Truncate
                let new_len = rng.next_usize(mutated.len() + 1);
                mutated.truncate(new_len);
            }
            2 => {
                // Extend
                let extra = 1 + rng.next_usize(64);
                for _ in 0..extra {
                    mutated.push((rng.next() & 0xFF) as u8);
                }
            }
            3 => {
                // Splice
                if !mutated.is_empty() {
                    let start = rng.next_usize(mutated.len());
                    let count = rng.next_usize(mutated.len() - start);
                    for byte in mutated.iter_mut().skip(start).take(count) {
                        *byte = (rng.next() & 0xFF) as u8;
                    }
                }
            }
            _ => unreachable!(),
        }

        let input_str = String::from_utf8_lossy(&mutated);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = codec::decode(&input_str, &codecs);
        }));
        if result.is_err() {
            panic!(
                "codec::decode panicked at iteration {iteration} with seed {seed:#x}, input: {input_str}"
            );
        }
    }
}
