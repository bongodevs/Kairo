use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Tremolo, TremoloConfig, Vibrato, VibratoConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn modulation(c: &mut Criterion) {
    let mut group = c.benchmark_group("modulation");
    group.bench_function("tremolo", |b| {
        let mut filter = Tremolo::new(TremoloConfig::default()).expect("tremolo");
        let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
    group.bench_function("vibrato", |b| {
        let mut filter = Vibrato::new(VibratoConfig::default()).expect("vibrato");
        let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
    group.finish();
}

criterion_group!(benches, modulation);
criterion_main!(benches);
