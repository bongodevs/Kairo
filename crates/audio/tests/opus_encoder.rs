use std::f32::consts::TAU;

use opus_rs::OpusDecoder;

use super::*;
use crate::decode::{AudioFormat, Millis, PcmBlock};
use crate::pcm::OUTPUT_FORMAT;
use crate::volume::Volume;
fn tone(frames: usize, phase: usize) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let time = (phase + frame) as f32 / SAMPLE_RATE as f32;
        let sample = (time * 440.0 * TAU).sin() * 0.4;
        samples.extend_from_slice(&[sample, -sample]);
    }
    samples
}

#[test]
fn arbitrary_blocks_assemble_frames_and_pad_only_the_final_one() {
    let samples = tone(2_000, 0);
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    let mut packets = Vec::new();
    let mut offset = 0;
    while offset < 2_000 {
        let frames = (2_000 - offset).min(333);
        encoder
            .process(
                PcmBlock {
                    samples: &samples[offset * CHANNELS..(offset + frames) * CHANNELS],
                    format: OUTPUT_FORMAT,
                    frames,
                    position: Millis::new(100 + (offset as u64 * 1_000 / u64::from(SAMPLE_RATE))),
                },
                |frame| {
                    packets.push((frame.packet.to_vec(), frame.position, frame.pcm_frames));
                },
            )
            .expect("encode block");
        offset += frames;
    }
    encoder
        .finish(|frame| {
            packets.push((frame.packet.to_vec(), frame.position, frame.pcm_frames));
        })
        .expect("finish");

    assert_eq!(packets.len(), 3);
    assert_eq!(packets[0].1, Millis::new(100));
    assert_eq!(packets[1].1, Millis::new(120));
    assert_eq!(packets[2].1, Millis::new(140));
    assert_eq!(packets.iter().map(|packet| packet.2).collect::<Vec<_>>(), [960, 960, 80]);
    assert!(
        packets.iter().all(|packet| !packet.0.is_empty() && packet.0.len() <= MAX_PACKET_BYTES)
    );
    assert_eq!(encoder.stats().frames, 3);
}

#[test]
fn exact_final_frame_does_not_emit_padding() {
    let samples = tone(FRAME_SAMPLES, 0);
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    let mut frames = Vec::new();
    encoder
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES,
                position: Millis::default(),
            },
            |frame| frames.push(frame.pcm_frames),
        )
        .expect("encode");
    encoder.finish(|frame| frames.push(frame.pcm_frames)).expect("finish");

    assert_eq!(frames, [FRAME_SAMPLES]);
}

#[test]
fn packets_round_trip_through_the_pure_rust_decoder() {
    let samples = tone(FRAME_SAMPLES * 8, 0);
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    let mut packets = Vec::new();
    encoder
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES * 8,
                position: Millis::default(),
            },
            |frame| packets.push(frame.packet.to_vec()),
        )
        .expect("encode");

    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder");
    let mut pcm = vec![0.0; FRAME_SAMPLES * CHANNELS];
    let mut energy = 0.0f64;
    for packet in packets {
        let frames = decoder.decode(&packet, FRAME_SAMPLES, &mut pcm).expect("decode packet");
        assert_eq!(frames, FRAME_SAMPLES);
        assert!(pcm.iter().all(|sample| sample.is_finite()));
        energy += pcm.iter().map(|sample| f64::from(*sample) * f64::from(*sample)).sum::<f64>();
    }
    assert!(energy > 1.0, "decoded stream should contain the tone");
}

#[test]
fn encoder_volume_changes_pcm_before_opus_encoding() {
    fn energy(volume: Volume) -> f64 {
        let samples = tone(FRAME_SAMPLES * 8, 0);
        let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
        encoder.set_volume_immediate(volume);
        let mut packets = Vec::new();
        encoder
            .process(
                PcmBlock {
                    samples: &samples,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES * 8,
                    position: Millis::default(),
                },
                |frame| packets.push(frame.packet.to_vec()),
            )
            .expect("encode");

        let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder");
        let mut pcm = vec![0.0; FRAME_SAMPLES * CHANNELS];
        packets
            .into_iter()
            .map(|packet| {
                decoder.decode(&packet, FRAME_SAMPLES, &mut pcm).expect("decode");
                pcm.iter().map(|sample| f64::from(*sample) * f64::from(*sample)).sum::<f64>()
            })
            .sum()
    }

    let unity = energy(Volume::UNITY);
    let quiet = energy(Volume::new(50));
    let silent = energy(Volume::SILENT);
    assert!(quiet < unity / 4.0);
    assert!(silent < 0.000_001);
}

#[test]
fn cbr_packet_size_tracks_the_configured_bitrate() {
    let config =
        OpusEncoderConfig { bitrate_bps: 128_000, use_cbr: true, ..OpusEncoderConfig::default() };
    let samples = tone(FRAME_SAMPLES, 0);
    let mut encoder = OpusFrameEncoder::new(config).expect("encoder");
    let mut packet_len = 0;
    encoder
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES,
                position: Millis::default(),
            },
            |frame| packet_len = frame.packet.len(),
        )
        .expect("encode");

    assert_eq!(packet_len, 320, "128 kbit/s for 20 ms is 320 bytes");
}

#[test]
fn vbr_packet_budget_increases_with_the_configured_bitrate() {
    fn encoded_bytes(bitrate_bps: i32) -> u64 {
        let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig {
            bitrate_bps,
            ..OpusEncoderConfig::default()
        })
        .expect("encoder");
        let samples = tone(FRAME_SAMPLES * 50, 0);
        encoder
            .process(
                PcmBlock {
                    samples: &samples,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES * 50,
                    position: Millis::default(),
                },
                |_| {},
            )
            .expect("encode");
        encoder.stats().bytes
    }

    let low = encoded_bytes(64_000);
    let medium = encoded_bytes(96_000);
    let high = encoded_bytes(128_000);
    assert!(low < medium, "{low} should be below {medium}");
    assert!(medium < high, "{medium} should be below {high}");
}

#[test]
fn packet_storage_is_reused() {
    let samples = tone(FRAME_SAMPLES * 4, 0);
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    let mut addresses = Vec::new();
    encoder
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES * 4,
                position: Millis::default(),
            },
            |frame| addresses.push(frame.packet.as_ptr() as usize),
        )
        .expect("encode");

    assert_eq!(addresses.len(), 4);
    assert!(addresses.iter().all(|address| *address == addresses[0]));
    assert_eq!(encoder.lookahead(), 312);
}

#[test]
fn invalid_settings_and_pcm_are_typed_errors() {
    let error = match OpusFrameEncoder::new(OpusEncoderConfig {
        complexity: 11,
        ..OpusEncoderConfig::default()
    }) {
        Ok(_) => panic!("complexity accepted"),
        Err(error) => error,
    };
    assert!(matches!(error, OpusError::InvalidComplexity));

    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    let error = encoder
        .process(
            PcmBlock {
                samples: &[0.0],
                format: AudioFormat { sample_rate: SAMPLE_RATE, channels: 1 },
                frames: 1,
                position: Millis::default(),
            },
            |_| {},
        )
        .expect_err("format rejected");
    assert!(matches!(error, OpusError::InvalidFormat { .. }));

    encoder.finish(|_| {}).expect("finish empty");
    let error = encoder
        .process(
            PcmBlock {
                samples: &[],
                format: OUTPUT_FORMAT,
                frames: 0,
                position: Millis::default(),
            },
            |_| {},
        )
        .expect_err("finished encoder rejects input");
    assert!(matches!(error, OpusError::Finished));
    encoder.reset(Millis::new(10));
    encoder
        .process(
            PcmBlock { samples: &[], format: OUTPUT_FORMAT, frames: 0, position: Millis::new(10) },
            |_| {},
        )
        .expect("reset accepts input");
}
