use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Rotation, RotationConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn rotation(c: &mut Criterion) {
    let mut filter = Rotation::new(RotationConfig { rotation_hz: 0.2 });
    let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
    c.bench_function("rotation", |b| {
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
}

criterion_group!(benches, rotation);
criterion_main!(benches);
