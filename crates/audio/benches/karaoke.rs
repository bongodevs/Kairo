use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{Karaoke, KaraokeConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn karaoke(c: &mut Criterion) {
    let mut group = c.benchmark_group("karaoke");
    for (name, config) in [
        ("level_zero", KaraokeConfig { level: 0.0, ..KaraokeConfig::default() }),
        ("default", KaraokeConfig::default()),
    ] {
        group.bench_function(name, |b| {
            let mut karaoke = Karaoke::new(config);
            let mut samples = [0.25f32; FRAME_SAMPLES * CHANNELS];
            b.iter(|| karaoke.process(std::hint::black_box(&mut samples)));
        });
    }
    group.finish();
}

criterion_group!(benches, karaoke);
criterion_main!(benches);
