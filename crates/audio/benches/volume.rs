use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::FilterVolume;
use kairo_audio::volume::Volume;
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn volume(c: &mut Criterion) {
    let mut group = c.benchmark_group("volume");
    for level in [0, 50, 100, 1_000] {
        group.bench_function(format!("{level}_percent"), |b| {
            let volume = Volume::new(level);
            let mut samples = [0.25f32; FRAME_SAMPLES * CHANNELS];
            b.iter(|| volume.apply(std::hint::black_box(&mut samples)));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("filter_volume");
    for level in [0.0, 0.5, 1.01, 2.0, 5.0] {
        group.bench_function(level.to_string(), |b| {
            let volume = FilterVolume::new(level).expect("filter volume");
            let mut samples = [0.25f32; FRAME_SAMPLES * CHANNELS];
            b.iter(|| volume.process(std::hint::black_box(&mut samples)));
        });
    }
    group.finish();
}

criterion_group!(benches, volume);
criterion_main!(benches);
