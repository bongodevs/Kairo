use std::f32::consts::TAU;
use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use opus_rs::{Application, OpusDecoder, OpusEncoder};

use kairo_audio::opus::inspect_packet;
use kairo_audio::{CHANNELS, FRAME_SAMPLES, MAX_PACKET_BYTES, SAMPLE_RATE};
fn packet() -> Vec<u8> {
    let mut pcm = Vec::with_capacity(FRAME_SAMPLES * CHANNELS);
    for frame in 0..FRAME_SAMPLES {
        let time = frame as f32 / SAMPLE_RATE as f32;
        let sample = (time * 440.0 * TAU).sin() * 0.4;
        pcm.extend_from_slice(&[sample, -sample]);
    }
    let mut encoder =
        OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio).expect("encoder");
    encoder.bitrate_bps = 99_000;
    let mut packet = [0u8; MAX_PACKET_BYTES];
    let length = encoder.encode(&pcm, FRAME_SAMPLES, &mut packet).expect("encode");
    packet[..length].to_vec()
}

fn opus_route(criterion: &mut Criterion) {
    let packet = packet();
    let mut group = criterion.benchmark_group("opus_route_48k_stereo_20ms");
    group.throughput(Throughput::Elements(1));
    group.bench_function("inspect_passthrough", |bencher| {
        bencher.iter(|| black_box(inspect_packet(black_box(&packet)).expect("inspect")));
    });

    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder");
    let mut pcm = vec![0.0; FRAME_SAMPLES * CHANNELS];
    group.bench_function("decode", |bencher| {
        bencher.iter(|| {
            black_box(decoder.decode(black_box(&packet), FRAME_SAMPLES, &mut pcm).expect("decode"))
        });
    });
    group.finish();
}

criterion_group!(benches, opus_route);
criterion_main!(benches);
