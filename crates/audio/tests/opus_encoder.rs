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

#[test]
fn pcm_guard_replaces_non_finite_samples_and_preserves_finite_bits() {
    let mut samples = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 2.5, -3.0, 0.5, -0.75, 0.0];
    sanitize_pcm(&mut samples);
    assert_eq!(samples[0], 0.0, "NaN should be replaced with 0.0");
    assert_eq!(samples[1], 0.0, "Infinity should be replaced with 0.0");
    assert_eq!(samples[2], 0.0, "-Infinity should be replaced with 0.0");
    assert_eq!(samples[3].to_bits(), 2.5f32.to_bits());
    assert_eq!(samples[4].to_bits(), (-3.0f32).to_bits());
    assert_eq!(samples[5], 0.5, "0.5 should be preserved");
    assert_eq!(samples[6], -0.75, "-0.75 should be preserved");
    assert_eq!(samples[7], 0.0, "0.0 should be preserved");
}

#[test]
fn unity_processing_keeps_every_finite_sample_bit_at_the_encoder() {
    let pattern = [-1.25, -1.0, -0.0, f32::MIN_POSITIVE, 0.25, 1.0, 1.125, 0.0];
    let samples = pattern.into_iter().cycle().take(FRAME_SAMPLES * CHANNELS).collect::<Vec<_>>();
    let expected = samples.iter().map(|sample| sample.to_bits()).collect::<Vec<_>>();
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    let mut emitted = false;

    encoder
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES,
                position: Millis::default(),
            },
            |_| emitted = true,
        )
        .expect("encode unity frame");

    assert!(emitted);
    assert_eq!(encoder.pcm.iter().map(|sample| sample.to_bits()).collect::<Vec<_>>(), expected);
}

#[test]
fn encoder_safely_encodes_nan_and_infinite_samples_without_panic() {
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    let mut bad_samples = vec![f32::NAN; FRAME_SAMPLES * CHANNELS];
    bad_samples[0] = f32::INFINITY;
    bad_samples[1] = f32::NEG_INFINITY;
    bad_samples[2] = 999.0;
    bad_samples[3] = -999.0;

    let mut packet = None;
    encoder
        .process(
            PcmBlock {
                samples: &bad_samples,
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES,
                position: Millis::default(),
            },
            |frame| {
                packet = Some(frame.packet.to_vec());
            },
        )
        .expect("encode bad block");

    let packet = packet.expect("frame packet produced");
    assert!(!packet.is_empty());

    // Verify the produced packet decodes without panic to all finite samples in [-1.0, 1.0]
    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder");
    let mut decoded = vec![0.0_f32; FRAME_SAMPLES * CHANNELS];
    let decoded_frames = decoder.decode(&packet, FRAME_SAMPLES, &mut decoded).expect("decode");
    assert_eq!(decoded_frames, FRAME_SAMPLES);
    for sample in &decoded[..decoded_frames * CHANNELS] {
        assert!(sample.is_finite(), "decoded sample must be finite: {sample}");
        assert!(*sample >= -1.05 && *sample <= 1.05, "decoded sample out of bounds: {sample}");
    }
}

#[test]
fn measure_pcm_guard_cost_per_frame() {
    let mut frame = [0.25_f32; FRAME_SAMPLES * CHANNELS];
    frame[10] = f32::NAN;
    frame[20] = 5.0;
    frame[30] = f32::INFINITY;

    // Warm up
    for _ in 0..1_000 {
        sanitize_pcm(&mut frame);
    }

    let iterations = 100_000;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        sanitize_pcm(&mut frame);
        std::hint::black_box(&frame);
    }
    let elapsed = start.elapsed();
    let nanos_per_frame = elapsed.as_nanos() as f64 / iterations as f64;
    println!(
        "\n  [SANITY GUARD BENCHMARK] {nanos_per_frame:.2} ns per 20ms frame ({:.4} µs)\n",
        nanos_per_frame / 1000.0
    );
    // Sanity guard must easily take less than 10 microseconds (< 0.05% of 20ms frame budget)
    assert!(nanos_per_frame < 10_000.0, "sanitize_pcm too slow: {nanos_per_frame} ns");
}
