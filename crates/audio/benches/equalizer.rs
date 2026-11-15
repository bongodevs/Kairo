use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Equalizer, EqualizerConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn equalizer(c: &mut Criterion) {
    let mut group = c.benchmark_group("equalizer");
    for (name, config) in [
        ("disabled", EqualizerConfig::default()),
        ("one_band", EqualizerConfig::from_bands([(7, 0.1)])),
        ("fifteen_bands", EqualizerConfig::from_bands((0..15).map(|band| (band, 0.1)))),
    ] {
        group.bench_function(name, |b| {
            let mut equalizer = Equalizer::new(config);
            let mut samples = [0.25f32; FRAME_SAMPLES * CHANNELS];
            b.iter(|| equalizer.process(std::hint::black_box(&mut samples)));
        });
    }
    group.finish();
}

criterion_group!(benches, equalizer);
criterion_main!(benches);
