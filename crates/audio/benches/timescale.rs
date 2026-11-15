use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Timescale, TimescaleConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE};
fn timescale(c: &mut Criterion) {
    let samples = tone();
    let mut group = c.benchmark_group("timescale");
    for (name, config) in [
        ("speed_1_25", TimescaleConfig { speed: 1.25, ..TimescaleConfig::default() }),
        ("pitch_1_25", TimescaleConfig { pitch: 1.25, ..TimescaleConfig::default() }),
        ("rate_1_25", TimescaleConfig { rate: 1.25, ..TimescaleConfig::default() }),
    ] {
        group.bench_function(name, |b| {
            let mut timescale = Timescale::new(config).expect("timescale");
            b.iter(|| {
                timescale
                    .process(std::hint::black_box(&samples), |output| {
                        std::hint::black_box(output);
                    })
                    .expect("process")
            });
        });
    }
    group.finish();
}

fn tone() -> [f32; FRAME_SAMPLES * CHANNELS] {
    let mut samples = [0.0; FRAME_SAMPLES * CHANNELS];
    for frame in 0..FRAME_SAMPLES {
        let sample = (frame as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin();
        samples[frame * CHANNELS] = sample;
        samples[frame * CHANNELS + 1] = -sample;
    }
    samples
}

criterion_group!(benches, timescale);
criterion_main!(benches);
