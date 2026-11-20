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

    let raw1 = codec::decode_base64_for_test(&encoded1).expect("valid base64");
    let raw2 = codec::decode_base64_for_test(&encoded2).expect("valid base64");
    let raw_seeds = [raw1, raw2];
    let string_seeds = [encoded1, encoded2, String::from("QAAAAA==")];

    let total = 50_000;
    let mut first_step_rejected = 0;
    let mut deep_path_reached = 0;

    for iteration in 0..total {
        let (input_str, is_structure_aware) = if rng.next_usize(100) < 75 {
            // Structure-aware binary mutation: mutate fields inside binary track, then validly base64 encode
            let base = &raw_seeds[rng.next_usize(raw_seeds.len())];
            let mut binary = base.clone();
            match rng.next_usize(6) {
                0 => {
                    // Mutate header (bytes 0..4): length or versioned bit
                    let offset = rng.next_usize(4);
                    binary[offset] ^= (rng.next() & 0xFF) as u8;
                }
                1 => {
                    // Mutate version byte (byte 4)
                    if binary.len() > 4 {
                        binary[4] = (rng.next() & 0x0F) as u8;
                    }
                }
                2 => {
                    // Mutate text length field or swap bytes in text region
                    if binary.len() > 10 {
                        let offset = 5 + rng.next_usize(binary.len() - 6);
                        binary[offset] ^= (rng.next() & 0xFF) as u8;
                    }
                }
                3 => {
                    // Swap sections
                    if binary.len() > 16 {
                        let a = 5 + rng.next_usize(binary.len() - 10);
                        let b = 5 + rng.next_usize(binary.len() - 10);
                        binary.swap(a, b);
                    }
                }
                4 => {
                    // Truncate binary within valid body
                    let new_len = 5 + rng.next_usize(binary.len() - 4);
                    binary.truncate(new_len);
                }
                5 => {
                    // Append extra payload bytes
                    let extra = 1 + rng.next_usize(32);
                    for _ in 0..extra {
                        binary.push((rng.next() & 0xFF) as u8);
                    }
                }
                _ => unreachable!(),
            }
            (codec::encode_base64_for_test(&binary), true)
        } else {
            // String-level mutation: byte flip, truncate, extend, splice
            let base = &string_seeds[rng.next_usize(string_seeds.len())];
            let mut bytes = base.clone().into_bytes();
            match rng.next_usize(4) {
                0 => {
                    let count = 1 + rng.next_usize(3);
                    for _ in 0..count {
                        if !bytes.is_empty() {
                            let idx = rng.next_usize(bytes.len());
                            bytes[idx] ^= (rng.next() & 0xFF) as u8;
                        }
                    }
                }
                1 => {
                    let new_len = rng.next_usize(bytes.len() + 1);
                    bytes.truncate(new_len);
                }
                2 => {
                    let extra = 1 + rng.next_usize(16);
                    for _ in 0..extra {
                        bytes.push((rng.next() & 0xFF) as u8);
                    }
                }
                3 => {
                    if !bytes.is_empty() {
                        let start = rng.next_usize(bytes.len());
                        let count = rng.next_usize(bytes.len() - start);
                        for byte in bytes.iter_mut().skip(start).take(count) {
                            *byte = (rng.next() & 0xFF) as u8;
                        }
                    }
                }
                _ => unreachable!(),
            }
            (String::from_utf8_lossy(&bytes).into_owned(), false)
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            codec::decode(&input_str, &codecs)
        }));

        match result {
            Ok(decode_result) => match decode_result {
                Err(TrackCodecError::InvalidBase64) => {
                    first_step_rejected += 1;
                }
                _ => {
                    deep_path_reached += 1;
                }
            },
            Err(_) => {
                panic!(
                    "codec::decode panicked at iteration {iteration} with seed {seed:#x}, structure_aware={is_structure_aware}, input: {input_str}"
                );
            }
        }
    }

    let deep_pct = (deep_path_reached as f64 / total as f64) * 100.0;
    println!(
        "Track codec: total={total}, first_step_rejected={first_step_rejected}, deep_path_reached={deep_path_reached} ({deep_pct:.2}%)"
    );
    assert!(deep_pct > 20.0, "Expected > 20% deep path reach, got {deep_pct:.2}%");
}
