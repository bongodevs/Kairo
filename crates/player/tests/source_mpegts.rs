use tokio::io::AsyncReadExt;

use super::*;

#[tokio::test]
async fn pat_pmt_and_pes_are_reduced_to_the_adts_elementary_stream() {
    let transport = crate::test_mpegts::mpeg_ts_adts();
    assert!(looks_like_ts(&transport));
    let mut stream = TsToAdts::new(std::io::Cursor::new(transport));
    let mut output = Vec::new();
    stream.read_to_end(&mut output).await.expect("extract ADTS");
    assert_eq!(output, crate::test_mpegts::adts_stream());
}

#[tokio::test]
async fn malformed_transport_sync_is_an_io_error() {
    let mut stream = TsToAdts::new(std::io::Cursor::new(vec![0; TS_PACKET_BYTES]));
    let mut output = Vec::new();
    assert!(stream.read_to_end(&mut output).await.is_err());
}

#[test]
#[ignore = "measures MPEG-TS to ADTS extraction in release mode"]
fn transport_extraction_has_a_measured_packet_cost() {
    const RUNS: usize = 10_000;
    let transport = crate::test_mpegts::mpeg_ts_adts();
    let packets = transport.len() / TS_PACKET_BYTES;
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let mut output = Vec::new();
    let started = std::time::Instant::now();
    runtime.block_on(async {
        for _ in 0..RUNS {
            output.clear();
            let mut stream = TsToAdts::new(std::io::Cursor::new(transport.as_slice()));
            stream.read_to_end(&mut output).await.expect("extract ADTS");
            std::hint::black_box(&output);
        }
    });
    let nanos = started.elapsed().as_nanos() / (RUNS * packets) as u128;
    eprintln!("MPEG-TS extraction: {nanos} ns/input packet");
    assert!(nanos < 5_000, "MPEG-TS extraction exceeded 5 µs per packet");
}
