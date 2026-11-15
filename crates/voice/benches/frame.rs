//! The per-frame hot path: the two calls the driver makes for every 20 ms frame, the RTP header
//! and the transport seal, on buffers that are allocated once outside the measured loop.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use voice::crypto::{KEY_LEN, MAX_PACKET_BYTES, TransportCipher, XCHACHA_MODE, sealed_len};
use voice::rtp::{RTP_HEADER_LEN, Rtp};
fn one_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame");
    // 20 ms of speech, of music, and the silence frame the tail is made of.
    for payload_len in [3usize, 60, 400, 1275] {
        let mut cipher = TransportCipher::new(XCHACHA_MODE, &[0x5au8; KEY_LEN]).unwrap();
        let mut rtp = Rtp::new(7);
        let payload = vec![0xa1u8; payload_len];
        let mut header = [0u8; RTP_HEADER_LEN];
        let mut datagram = vec![0u8; sealed_len(RTP_HEADER_LEN, payload_len)];
        group.bench_function(BenchmarkId::from_parameter(payload_len), |b| {
            b.iter(|| {
                rtp.next_header(&mut header, false);
                cipher.seal(&header, &payload, &mut datagram).unwrap()
            });
        });
    }
    group.finish();
}

fn buffers_stay_where_they_are(c: &mut Criterion) {
    // The sender's frame buffer is one fixed array: touching it must not allocate.
    let mut group = c.benchmark_group("buffer");
    group.bench_function("copy-a-max-size-frame", |b| {
        let source = vec![0xa1u8; MAX_PACKET_BYTES];
        let mut frame = [0u8; MAX_PACKET_BYTES];
        b.iter(|| {
            frame[..source.len()].copy_from_slice(&source);
            frame.len()
        });
    });
    group.finish();
}

criterion_group!(benches, one_frame, buffers_stay_where_they_are);
criterion_main!(benches);
