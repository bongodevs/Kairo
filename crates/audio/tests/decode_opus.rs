use std::f32::consts::TAU;
use std::io::Cursor;
use std::sync::Arc;

use opus_rs::{Application, OpusEncoder};
use tokio::runtime::Handle;

use kairo_core::{OpenedSource, SourceHints, SourceReference};

use super::*;
use crate::opus::OpusRoute;
use crate::test_ogg;
use crate::{CHANNELS, MAX_PACKET_BYTES, SAMPLE_RATE};
fn packets(channels: usize, frame_samples: usize, count: usize) -> Vec<Vec<u8>> {
    let mut encoder =
        OpusEncoder::new(SAMPLE_RATE as i32, channels, Application::Audio).expect("encoder");
    encoder.bitrate_bps = 99_000;
    encoder.complexity = 5;
    let mut packets = Vec::new();
    for packet_index in 0..count {
        let mut pcm = Vec::with_capacity(frame_samples * channels);
        for frame in 0..frame_samples {
            let time = (packet_index * frame_samples + frame) as f32 / SAMPLE_RATE as f32;
            let sample = (time * 440.0 * TAU).sin() * 0.4;
            for channel in 0..channels {
                pcm.push(if channel == 0 { sample } else { -sample });
            }
        }
        let mut output = [0u8; MAX_PACKET_BYTES];
        let length = encoder.encode(&pcm, frame_samples, &mut output).expect("encode");
        packets.push(output[..length].to_vec());
    }
    packets
}

fn large_padded_packet() -> Vec<u8> {
    let mut encoder =
        OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio).expect("encoder");
    encoder.bitrate_bps = 64_000;
    encoder.complexity = 5;
    let mut frames = Vec::new();
    for packet_index in 0..2 {
        let mut pcm = Vec::with_capacity(480 * CHANNELS);
        for frame in 0..480 {
            let time = (packet_index * 480 + frame) as f32 / SAMPLE_RATE as f32;
            let sample = (time * 440.0 * TAU).sin() * 0.4;
            pcm.extend_from_slice(&[sample, -sample]);
        }
        let mut output = [0; MAX_PACKET_BYTES];
        let length = encoder.encode(&pcm, 480, &mut output).expect("encode");
        frames.push(output[..length].to_vec());
    }
    assert_eq!(frames[0][0], frames[1][0]);
    let first = &frames[0][1..];
    let second = &frames[1][1..];
    assert!(first.len() < 252);
    let padding = 1_200;
    let mut packet = vec![frames[0][0] | 3, 0xc2];
    let mut remaining = padding;
    while remaining >= 254 {
        packet.push(255);
        remaining -= 254;
    }
    packet.push(remaining as u8);
    packet.push(first.len() as u8);
    packet.extend_from_slice(first);
    packet.extend_from_slice(second);
    packet.resize(packet.len() + padding, 0);
    assert!(packet.len() > MAX_PACKET_BYTES);
    assert!(packet.len() <= crate::MAX_SOURCE_PACKET_BYTES);
    packet
}

fn source(packets: &[Vec<u8>], channels: u8, pre_skip: u16) -> OpenedSource {
    let packet_refs = packets.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let bytes: Arc<[u8]> = test_ogg::opus_stream(&packet_refs, channels, pre_skip).into();
    OpenedSource::seekable(
        "memory",
        SourceReference::new("track.ogg"),
        SourceHints {
            mime_type: Some(String::from("audio/ogg")),
            file_extension: Some(String::from("ogg")),
        },
        Some(u64::try_from(bytes.len()).expect("fixture length")),
        Cursor::new(bytes),
    )
}

#[tokio::test]
async fn identity_processing_passes_twenty_ms_stereo_without_a_copy() {
    let packets = packets(CHANNELS, 960, 2);
    let expected = packets[0].clone();
    let runtime = Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(&packets, 2, 0), runtime).expect("open");
        assert_eq!(decoder.info().codec, "opus");
        let (packet, frames) =
            match decoder.next_audio(OpusRoute::default()).expect("next packet").expect("packet") {
                AudioBlock::Opus(block) => (block.packet.to_vec(), block.info.total_samples),
                AudioBlock::Pcm(_) => panic!("identity route decoded"),
            };
        let decoded_frames = match decoder
            .next_audio(OpusRoute { volume: 99, filters_active: false })
            .expect("next decoded")
            .expect("decoded")
        {
            AudioBlock::Pcm(block) => block.frames,
            AudioBlock::Opus(_) => panic!("processing route passed through"),
        };
        (packet, frames, decoded_frames, decoder.stats())
    })
    .await
    .expect("worker");

    assert_eq!(result.0, expected);
    assert_eq!(result.1, 960);
    assert_eq!(result.2, 960);
    assert_eq!(result.3.passthrough_packets, 1);
    assert_eq!(result.3.decoded_packets, 1);
}

#[tokio::test]
async fn mono_or_short_packets_are_decoded_even_when_processing_is_identity() {
    let mono = packets(1, 960, 1);
    let short = packets(CHANNELS, 480, 1);
    let runtime = Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        let mut mono_decoder =
            BlockingDecoder::open(source(&mono, 1, 0), runtime.clone()).expect("mono open");
        let mono_format = match mono_decoder
            .next_audio(OpusRoute::default())
            .expect("mono next")
            .expect("mono block")
        {
            AudioBlock::Pcm(block) => block.format,
            AudioBlock::Opus(_) => panic!("mono passed through"),
        };

        let mut short_decoder =
            BlockingDecoder::open(source(&short, 2, 0), runtime).expect("short open");
        let short_frames = match short_decoder
            .next_audio(OpusRoute::default())
            .expect("short next")
            .expect("short block")
        {
            AudioBlock::Pcm(block) => block.frames,
            AudioBlock::Opus(_) => panic!("10 ms passed through"),
        };
        (mono_format, short_frames)
    })
    .await
    .expect("worker");

    assert_eq!(result.0.channels, 1);
    assert_eq!(result.0.sample_rate, SAMPLE_RATE);
    assert_eq!(result.1, 480);
}

#[tokio::test]
async fn pre_skip_is_removed_before_passthrough_can_begin() {
    let packets = packets(CHANNELS, 960, 2);
    let runtime = Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(&packets, 2, 312), runtime).expect("open");
        let (first_frames, first_position) =
            match decoder.next_audio(OpusRoute::default()).expect("first").expect("first block") {
                AudioBlock::Pcm(block) => (block.frames, block.position),
                AudioBlock::Opus(_) => panic!("untrimmed packet passed through"),
            };
        let second_is_opus = matches!(
            decoder.next_audio(OpusRoute::default()).expect("second"),
            Some(AudioBlock::Opus(_))
        );
        (first_frames, first_position, second_is_opus)
    })
    .await
    .expect("worker");

    assert_eq!(result.0, 648);
    assert_eq!(result.1, Millis::default());
    assert!(result.2);
}

#[tokio::test]
async fn malformed_packets_are_counted_and_skipped() {
    let packets = vec![vec![0xfd, 1]];
    let runtime = Handle::current();
    let stats = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(&packets, 2, 0), runtime).expect("open");
        assert!(decoder.next_audio(OpusRoute::default()).expect("next").is_none());
        decoder.stats()
    })
    .await
    .expect("worker");

    assert_eq!(stats.skipped_packets, 1);
}

#[tokio::test]
async fn large_source_packet_decodes_instead_of_entering_voice_passthrough() {
    let packets = vec![large_padded_packet()];
    let runtime = Handle::current();
    let stats = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(&packets, 2, 0), runtime).expect("open");
        let block = decoder.next_audio(OpusRoute::default()).expect("next").expect("decoded block");
        assert!(matches!(block, AudioBlock::Pcm(block) if block.frames == 960));
        decoder.stats()
    })
    .await
    .expect("worker");
    assert_eq!(stats.passthrough_packets, 0);
    assert_eq!(stats.decoded_packets, 1);
}
