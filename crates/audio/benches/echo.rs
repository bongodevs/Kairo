use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Echo, EchoConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn echo(c: &mut Criterion) {
    let mut filter = Echo::new(EchoConfig { length_seconds: 0.3, decay: 0.5 }).expect("echo");
    let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
    c.bench_function("echo", |b| {
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
}

criterion_group!(benches, echo);
criterion_main!(benches);
