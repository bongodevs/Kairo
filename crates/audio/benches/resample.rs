use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use kairo_audio::decode::{AudioFormat, Millis, PcmBlock};
use kairo_audio::pcm::{PcmNormalizer, ResamplingQuality};
const INPUT_RATE: u32 = 44_100;
const CHANNELS: usize = 2;
const BLOCK_FRAMES: usize = 1_024;

fn run(normalizer: &mut PcmNormalizer, input: &[f32]) -> usize {
    let format = AudioFormat { sample_rate: INPUT_RATE, channels: CHANNELS };
    let mut emitted = 0;
    for samples in input.chunks(BLOCK_FRAMES * CHANNELS) {
        let frames = samples.len() / CHANNELS;
        normalizer
            .process(PcmBlock { samples, format, frames, position: Millis::default() }, |block| {
                emitted += block.frames;
                black_box(block.samples);
            })
            .expect("resample block");
    }
    normalizer
        .finish(|block| {
            emitted += block.frames;
            black_box(block.samples);
        })
        .expect("drain resampler");
    emitted
}

fn resample(criterion: &mut Criterion) {
    let frames = INPUT_RATE as usize;
    let mut input = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let sample = (frame as f32 * 0.01).sin() * 0.5;
        input.extend_from_slice(&[sample, -sample]);
    }

    let mut group = criterion.benchmark_group("resample_44k1_to_48k_stereo");
    group.throughput(Throughput::Elements(frames as u64));
    for quality in [ResamplingQuality::Low, ResamplingQuality::Medium, ResamplingQuality::High] {
        let mut normalizer = PcmNormalizer::new(quality);
        let _ = run(&mut normalizer, &input);
        group.bench_function(format!("{quality:?}").to_lowercase(), |bencher| {
            bencher.iter(|| {
                normalizer.reset(Millis::default());
                black_box(run(&mut normalizer, &input))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, resample);
criterion_main!(benches);
