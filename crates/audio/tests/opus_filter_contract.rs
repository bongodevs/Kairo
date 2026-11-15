use super::*;
use crate::decode::{Millis, PcmBlock};
use crate::filters::{
    Distortion, DistortionConfig, EchoConfig, Filters, KaraokeConfig, LowPass, LowPassConfig,
};
use crate::pcm::OUTPUT_FORMAT;
#[test]
fn final_padding_does_not_advance_stateful_filters() {
    let plain = final_packet(Filters::default());
    let filtered = final_packet(Filters {
        echo: Some(EchoConfig { length_seconds: 1.5 / SAMPLE_RATE as f32, decay: 1.0 }),
        ..Filters::default()
    });
    assert_eq!(filtered, plain);
}

#[test]
fn seek_preserves_karaoke_history() {
    assert_eq!(second_karaoke_packet(false), second_karaoke_packet(true));
}

#[test]
fn filters_run_in_the_reference_signal_order() {
    let low_pass = LowPassConfig { smoothing: 2.0 };
    let distortion = DistortionConfig {
        sin_scale: 1.5,
        cos_scale: 0.5,
        tan_scale: 0.5,
        ..DistortionConfig::default()
    };

    let mut reference = stereo_tone(FRAME_SAMPLES);
    LowPass::new(low_pass).process(&mut reference);
    Distortion::new(distortion).process(&mut reference);

    let mut reversed = stereo_tone(FRAME_SAMPLES);
    Distortion::new(distortion).process(&mut reversed);
    LowPass::new(low_pass).process(&mut reversed);

    let expected = encode_once(&reference);
    let filtered = encode_once_with(
        &stereo_tone(FRAME_SAMPLES),
        Filters { low_pass: Some(low_pass), distortion: Some(distortion), ..Filters::default() },
    );

    assert_eq!(filtered, expected, "the encoder must run low-pass before distortion");
    assert_ne!(encode_once(&reversed), expected, "the two orders must be observably different");
}

fn encode_once(samples: &[f32]) -> Vec<u8> {
    encode_once_maybe(samples, None)
}

fn encode_once_with(samples: &[f32], filters: Filters) -> Vec<u8> {
    encode_once_maybe(samples, Some(filters))
}

fn encode_once_maybe(samples: &[f32], filters: Option<Filters>) -> Vec<u8> {
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    if let Some(filters) = filters {
        encoder.set_filters(filters).expect("filters");
    }
    let mut packet = None;
    encoder
        .process(
            PcmBlock {
                samples,
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES,
                position: Millis::default(),
            },
            |frame| packet = Some(frame.packet.to_vec()),
        )
        .expect("frame");
    encoder.finish(|_| panic!("a complete frame must not leave a partial one")).expect("finish");
    packet.expect("packet")
}

fn final_packet(filters: Filters) -> Vec<u8> {
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    encoder.set_filters(filters).expect("filters");
    encoder
        .process(
            PcmBlock {
                samples: &[0.5, -0.5],
                format: OUTPUT_FORMAT,
                frames: 1,
                position: Millis::default(),
            },
            |_| panic!("partial frame emitted before finish"),
        )
        .expect("partial input");
    let mut packet = None;
    encoder.finish(|frame| packet = Some(frame.packet.to_vec())).expect("finish");
    packet.expect("final packet")
}

fn second_karaoke_packet(seek: bool) -> Vec<u8> {
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
    encoder
        .set_filters(Filters { karaoke: Some(KaraokeConfig::default()), ..Filters::default() })
        .expect("karaoke");
    let samples = stereo_tone(FRAME_SAMPLES * 2);
    let mut packets = Vec::new();
    encoder
        .process(
            PcmBlock {
                samples: &samples[..FRAME_SAMPLES * CHANNELS],
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES,
                position: Millis::default(),
            },
            |frame| packets.push(frame.packet.to_vec()),
        )
        .expect("first frame");
    if seek {
        encoder.reset(Millis::new(20));
    }
    encoder
        .process(
            PcmBlock {
                samples: &samples[FRAME_SAMPLES * CHANNELS..],
                format: OUTPUT_FORMAT,
                frames: FRAME_SAMPLES,
                position: Millis::new(20),
            },
            |frame| packets.push(frame.packet.to_vec()),
        )
        .expect("second frame");
    packets.pop().expect("second packet")
}

fn stereo_tone(frames: usize) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let phase = frame as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32;
        let sample = phase.sin() * 0.4;
        samples.extend_from_slice(&[sample, sample * 0.25]);
    }
    samples
}
