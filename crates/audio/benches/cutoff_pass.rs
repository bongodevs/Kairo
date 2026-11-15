use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{CutoffLowPass, HighPass, PassConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn cutoff_pass(c: &mut Criterion) {
    let config = PassConfig { cutoff_hz: 1_000, boost: 1.0 };
    let mut group = c.benchmark_group("cutoff_pass");
    group.bench_function("high_pass", |b| {
        let mut filter = HighPass::new(config);
        let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
    group.bench_function("low_pass", |b| {
        let mut filter = CutoffLowPass::new(config);
        let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
    group.finish();
}

criterion_group!(benches, cutoff_pass);
criterion_main!(benches);
