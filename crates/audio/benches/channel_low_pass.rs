use criterion::{Criterion, criterion_group, criterion_main};

use kairo_audio::filters::{ChannelMix, ChannelMixConfig, LowPass, LowPassConfig};
use kairo_audio::{CHANNELS, FRAME_SAMPLES};
fn channel_low_pass(c: &mut Criterion) {
    let mut group = c.benchmark_group("channel_low_pass");
    group.bench_function("channel_mix", |b| {
        let filter = ChannelMix::new(ChannelMixConfig {
            left_to_left: 0.5,
            left_to_right: 0.5,
            right_to_left: 0.5,
            right_to_right: 0.5,
        });
        let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
    group.bench_function("low_pass", |b| {
        let mut filter = LowPass::new(LowPassConfig::default());
        let mut samples = [0.25; FRAME_SAMPLES * CHANNELS];
        b.iter(|| filter.process(std::hint::black_box(&mut samples)));
    });
    group.finish();
}

criterion_group!(benches, channel_low_pass);
criterion_main!(benches);
