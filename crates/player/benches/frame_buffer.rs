use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use player::audio::{FRAME_SAMPLES, MAX_PACKET_BYTES};
use player::buffer::{BufferRead, BufferedFrame, FrameBufferConfig, frame_buffer};
use player::decode::Millis;
fn frame_buffer_handoff(criterion: &mut Criterion) {
    let config = FrameBufferConfig::new(200).expect("config");
    let (mut producer, mut consumer) = frame_buffer(config);
    let packet = [0x5a; 250];
    let mut output = [0u8; MAX_PACKET_BYTES];
    let mut position = 0u64;
    let mut group = criterion.benchmark_group("frame_buffer");
    group.throughput(Throughput::Elements(1));
    group.bench_function("push_pop_250_bytes", |bencher| {
        bencher.iter(|| {
            let frame =
                BufferedFrame::new(&packet, Millis::new(position), FRAME_SAMPLES).expect("frame");
            producer.blocking_send(frame).expect("push");
            let read = consumer.read(&mut output).expect("pop");
            assert!(matches!(read, BufferRead::Frame(_)));
            position = position.wrapping_add(20);
            black_box(&output[..packet.len()]);
        });
    });
    group.finish();
}

criterion_group!(benches, frame_buffer_handoff);
criterion_main!(benches);
