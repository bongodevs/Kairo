use std::f32::consts::TAU;

use super::*;
use crate::decode::{Millis, PcmBlock};
use crate::filters::{Filters, TimescaleConfig};
use crate::pcm::OUTPUT_FORMAT;
#[test]
fn timescale_metadata_advances_on_the_effective_source_timeline() {
    for (config, step) in [
        (TimescaleConfig { speed: 2.0, ..TimescaleConfig::default() }, 40),
        (TimescaleConfig { pitch: 2.0, ..TimescaleConfig::default() }, 20),
        (TimescaleConfig { rate: 2.0, ..TimescaleConfig::default() }, 40),
    ] {
        let positions = encode_positions(config);
        assert!(positions.len() > 5);
        assert_eq!(positions[0].0, 100);
        let (last, complete) = positions.split_last().expect("encoded positions");
        assert!(
            complete.iter().all(|(position, next)| next - position == step),
            "{positions:?} expected {step} ms steps"
        );
        assert!(last.1 > last.0 && last.1 - last.0 <= step);
        assert!(positions.windows(2).all(|pair| pair[0].1 == pair[1].0));
    }
}

#[test]
fn an_identity_timescale_keeps_native_opus_eligible_for_passthrough() {
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    encoder
        .set_filters(Filters { timescale: Some(TimescaleConfig::default()), ..Filters::default() })
        .expect("identity timescale");
    assert!(!encoder.filters_active());
    assert!(!encoder.has_pending_pcm());
}

#[test]
fn a_final_partial_frame_reports_only_its_real_source_duration() {
    let samples = tone(80);
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    encoder
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames: 80,
                position: Millis::new(100),
            },
            |_| {},
        )
        .expect("process");
    let mut positions = None;
    encoder
        .finish(|frame| positions = Some((frame.position, frame.next_position)))
        .expect("finish");
    assert_eq!(positions, Some((Millis::new(100), Millis::new(101))));
}

fn encode_positions(config: TimescaleConfig) -> Vec<(u64, u64)> {
    let frames = FRAME_SAMPLES * 20;
    let samples = tone(frames);
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    encoder
        .set_filters(Filters { timescale: Some(config), ..Filters::default() })
        .expect("timescale configuration");
    let mut positions = Vec::new();
    encoder
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames,
                position: Millis::new(100),
            },
            |frame| positions.push((frame.position.get(), frame.next_position.get())),
        )
        .expect("process");
    encoder
        .finish(|frame| positions.push((frame.position.get(), frame.next_position.get())))
        .expect("finish");
    positions
}

fn tone(frames: usize) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let sample = (frame as f32 * 440.0 * TAU / SAMPLE_RATE as f32).sin() * 0.25;
        samples.extend_from_slice(&[sample, -sample]);
    }
    samples
}
