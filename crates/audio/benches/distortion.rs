use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Distortion, DistortionConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn distortion(c: &mut Criterion) {
    let filter = Distortion::new(DistortionConfig {
        sin_offset: 0.2,
        cos_scale: 0.8,
        tan_scale: 0.5,
        ..DistortionConfig::default()
    });
    let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
    c.bench_function("distortion", |b| {
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
}

criterion_group!(benches, distortion);
criterion_main!(benches);
