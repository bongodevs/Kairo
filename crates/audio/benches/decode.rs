use std::hint::black_box;
use std::io::Cursor;
use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use tokio::runtime::Builder;

use kairo_audio::decode::BlockingDecoder;
use kairo_core::{OpenedSource, SourceHints, SourceReference};
#[path = "../tests/support_wav.rs"]
mod wav;

fn decode_wave(criterion: &mut Criterion) {
    let frames = 48_000 * 5;
    let bytes: Arc<[u8]> = wav::pcm_wave(48_000, 2, frames).into();
    let length = u64::try_from(bytes.len()).expect("fixture length");
    let runtime =
        Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("runtime");
    let handle = runtime.handle().clone();
    let mut group = criterion.benchmark_group("decode");
    group.throughput(Throughput::Elements(u64::from(frames)));
    group.bench_function("pcm_s16le_stereo_48k", |bencher| {
        bencher.iter(|| {
            let source = OpenedSource::seekable(
                "memory",
                SourceReference::new("bench.wav"),
                SourceHints {
                    mime_type: Some(String::from("audio/wav")),
                    file_extension: Some(String::from("wav")),
                },
                Some(length),
                Cursor::new(bytes.clone()),
            );
            let mut decoder = BlockingDecoder::open(source, handle.clone()).expect("open decoder");
            let mut decoded = 0usize;
            while let Some(block) = decoder.next_block().expect("decode block") {
                decoded += block.frames;
                black_box(block.samples);
            }
            black_box(decoded)
        });
    });
    group.finish();
}

criterion_group!(benches, decode_wave);
criterion_main!(benches);
