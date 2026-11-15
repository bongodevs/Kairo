use std::f32::consts::TAU;
use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use kairo_audio::decode::{Millis, PcmBlock};
use kairo_audio::opus::{
    CHANNELS, FRAME_SAMPLES, OpusEncoderConfig, OpusFrameEncoder, SAMPLE_RATE,
};
use kairo_audio::pcm::OUTPUT_FORMAT;
fn opus_encode(criterion: &mut Criterion) {
    let mut pcm = Vec::with_capacity(FRAME_SAMPLES * CHANNELS);
    for frame in 0..FRAME_SAMPLES {
        let time = frame as f32 / SAMPLE_RATE as f32;
        let sample = (time * 440.0 * TAU).sin() * 0.4;
        pcm.extend_from_slice(&[sample, -sample]);
    }
    let block = || PcmBlock {
        samples: &pcm,
        format: OUTPUT_FORMAT,
        frames: FRAME_SAMPLES,
        position: Millis::default(),
    };

    let mut group = criterion.benchmark_group("opus_encode_48k_stereo_20ms_vbr");
    group.throughput(Throughput::Elements(1));
    for bitrate_bps in [64_000, 96_000, 128_000] {
        let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig {
            bitrate_bps,
            ..OpusEncoderConfig::default()
        })
        .expect("encoder");
        group.bench_function(format!("{}k", bitrate_bps / 1_000), |bencher| {
            bencher.iter(|| {
                encoder
                    .process(block(), |frame| {
                        black_box(frame.packet);
                    })
                    .expect("encode frame")
            });
        });
    }
    group.finish();

    let mut group = criterion.benchmark_group("opus_encode_48k_stereo_99k_complexity");
    group.throughput(Throughput::Elements(1));
    for complexity in [0, 5, 10] {
        let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig {
            bitrate_bps: 99_000,
            complexity,
            ..OpusEncoderConfig::default()
        })
        .expect("encoder");
        group.bench_function(complexity.to_string(), |bencher| {
            bencher.iter(|| {
                encoder
                    .process(block(), |frame| {
                        black_box(frame.packet);
                    })
                    .expect("encode frame")
            });
        });
    }
    group.finish();
}

criterion_group!(benches, opus_encode);
criterion_main!(benches);
