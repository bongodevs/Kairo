use super::*;
use crate::decode::{Millis, PcmBlock};
use crate::filters::{
    ChannelMixConfig, Distortion, DistortionConfig, EchoConfig, EqualizerConfig, Filters,
    KaraokeConfig, LowPass, LowPassConfig, NormalizationConfig, PassConfig, RotationConfig,
    TimescaleConfig, TremoloConfig, VibratoConfig,
};
use crate::pcm::OUTPUT_FORMAT;
#[test]
fn final_padding_does_not_advance_stateful_filters() {
    let plain = final_packet(Filters::default());
    let filtered = final_packet(Filters {
        echo: Some(EchoConfig { length_seconds: 1.5 / SAMPLE_RATE as f32, decay: 0.5 }),
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

struct TestRng(u64);

impl TestRng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }

    fn sample_f32(&mut self) -> f32 {
        match self.next_u64() % 12 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => f32::NEG_INFINITY,
            3 => 0.0,
            4 => -0.0,
            5 => -1.0,
            6 => 1.0,
            7 => -1e30,
            8 => 1e30,
            9 => -1e-30,
            10 => ((self.next_u64() as i32) as f32) / 10_000.0,
            _ => (self.next_u64() % 100) as f32 * 0.05,
        }
    }

    fn sample_f64(&mut self) -> f64 {
        match self.next_u64() % 8 {
            0 => f64::NAN,
            1 => f64::INFINITY,
            2 => f64::NEG_INFINITY,
            3 => 0.0,
            4 => -1e30,
            5 => 1e30,
            _ => f64::from(self.sample_f32()),
        }
    }
}

#[test]
fn property_test_filter_random_values_never_panic_and_produce_finite_output() {
    let mut rng = TestRng(0xDEAD_BEEF_CAFE_BABE);
    let test_pcm = stereo_tone(FRAME_SAMPLES);

    for _ in 0..2_000 {
        let volume = rng.sample_f32();
        let karaoke = rng.next_u64().is_multiple_of(2).then(|| KaraokeConfig {
            level: rng.sample_f32(),
            mono_level: rng.sample_f32(),
            filter_band: rng.sample_f32(),
            filter_width: rng.sample_f32(),
        });
        let timescale = rng.next_u64().is_multiple_of(2).then(|| TimescaleConfig {
            speed: rng.sample_f64(),
            pitch: rng.sample_f64(),
            rate: rng.sample_f64(),
        });
        let tremolo = rng
            .next_u64()
            .is_multiple_of(2)
            .then(|| TremoloConfig { frequency: rng.sample_f32(), depth: rng.sample_f32() });
        let vibrato = rng
            .next_u64()
            .is_multiple_of(2)
            .then(|| VibratoConfig { frequency: rng.sample_f32(), depth: rng.sample_f32() });
        let distortion = rng.next_u64().is_multiple_of(2).then(|| DistortionConfig {
            sin_offset: rng.sample_f32(),
            sin_scale: rng.sample_f32(),
            cos_offset: rng.sample_f32(),
            cos_scale: rng.sample_f32(),
            tan_offset: rng.sample_f32(),
            tan_scale: rng.sample_f32(),
            offset: rng.sample_f32(),
            scale: rng.sample_f32(),
        });
        let rotation = rng
            .next_u64()
            .is_multiple_of(2)
            .then(|| RotationConfig { rotation_hz: rng.sample_f64() });
        let channel_mix = rng.next_u64().is_multiple_of(2).then(|| ChannelMixConfig {
            left_to_left: rng.sample_f32(),
            left_to_right: rng.sample_f32(),
            right_to_left: rng.sample_f32(),
            right_to_right: rng.sample_f32(),
        });
        let low_pass =
            rng.next_u64().is_multiple_of(2).then(|| LowPassConfig { smoothing: rng.sample_f32() });
        let high_pass = rng.next_u64().is_multiple_of(2).then(|| PassConfig {
            cutoff_hz: (rng.next_u64() % 24_000) as u32,
            boost: rng.sample_f32(),
        });
        let cutoff_low_pass = rng.next_u64().is_multiple_of(2).then(|| PassConfig {
            cutoff_hz: (rng.next_u64() % 24_000) as u32,
            boost: rng.sample_f32(),
        });
        let normalization = rng.next_u64().is_multiple_of(2).then(|| NormalizationConfig {
            max_amplitude: rng.sample_f32(),
            adaptive: rng.next_u64().is_multiple_of(2),
        });
        let echo = rng
            .next_u64()
            .is_multiple_of(2)
            .then(|| EchoConfig { length_seconds: rng.sample_f32(), decay: rng.sample_f32() });

        let bands = (0..15).map(|band| (band, rng.sample_f32()));
        let equalizer = EqualizerConfig::from_bands(bands);

        let filters = Filters {
            volume,
            equalizer,
            karaoke,
            timescale,
            tremolo,
            vibrato,
            distortion,
            rotation,
            channel_mix,
            low_pass,
            high_pass,
            cutoff_low_pass,
            normalization,
            echo,
        };

        let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
        let validation = filters.validate();

        match encoder.set_filters(filters) {
            Ok(()) => {
                assert!(validation.is_ok(), "set_filters succeeded but validation failed");
                let mut emitted_packets = Vec::new();
                let process_result = encoder.process(
                    PcmBlock {
                        samples: &test_pcm,
                        format: OUTPUT_FORMAT,
                        frames: FRAME_SAMPLES,
                        position: Millis::default(),
                    },
                    |frame| emitted_packets.push(frame.packet.to_vec()),
                );
                assert!(process_result.is_ok(), "process failed on valid filters");

                // Decode emitted packets and assert all decoded samples are finite
                let mut decoder =
                    opus_rs::OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder");
                let mut pcm_buf = vec![0.0_f32; FRAME_SAMPLES * CHANNELS];
                for packet in emitted_packets {
                    let decoded =
                        decoder.decode(&packet, FRAME_SAMPLES, &mut pcm_buf).expect("decode");
                    for sample in &pcm_buf[..decoded * CHANNELS] {
                        assert!(sample.is_finite(), "decoded sample was non-finite: {sample}");
                        assert!(*sample >= -1.05 && *sample <= 1.05, "sample clamped: {sample}");
                    }
                }
            }
            Err(_) => {
                // When rejected, validation must also have failed
                assert!(validation.is_err(), "set_filters rejected but validation succeeded");
            }
        }
    }
}
