use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Normalization, NormalizationConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn normalization(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalization");
    for (name, adaptive) in [("adaptive", true), ("frame_local", false)] {
        group.bench_function(name, |b| {
            let mut filter =
                Normalization::new(NormalizationConfig { max_amplitude: 0.5, adaptive });
            let mut samples = [0.75; FRAME_SAMPLES * CHANNELS];
            b.iter(|| {
                samples.fill(0.75);
                filter.process(std::hint::black_box(&mut samples));
            });
        });
    }
    group.finish();
}

criterion_group!(benches, normalization);
criterion_main!(benches);
