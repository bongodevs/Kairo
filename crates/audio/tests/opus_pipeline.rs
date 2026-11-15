use std::io::Cursor;
use std::sync::Arc;

use tokio::runtime::Handle;

use kairo_core::{OpenedSource, SourceHints, SourceReference};

use super::*;
use crate::decode::BlockingDecoder;
use crate::pcm::{PcmNormalizer, ResamplingQuality};
use crate::test_wav as wav;
#[tokio::test]
async fn source_decode_resample_and_encode_run_as_one_worker_pipeline() {
    let runtime = Handle::current();
    let bytes: Arc<[u8]> = wav::pcm_wave(44_100, 1, 4_410).into();
    let length = u64::try_from(bytes.len()).expect("fixture length");
    let packets = tokio::task::spawn_blocking(move || {
        let source = OpenedSource::seekable(
            "memory",
            SourceReference::new("pipeline.wav"),
            SourceHints {
                mime_type: Some(String::from("audio/wav")),
                file_extension: Some(String::from("wav")),
            },
            Some(length),
            Cursor::new(bytes),
        );
        let mut decoder = BlockingDecoder::open(source, runtime).expect("decoder");
        let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
        let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig::default()).expect("encoder");
        let mut packets = Vec::new();
        while let Some(block) = decoder.next_block().expect("decode") {
            normalizer
                .process(block, |block| {
                    encoder
                        .process(block, |frame| packets.push(frame.packet.to_vec()))
                        .expect("encode");
                })
                .expect("normalize");
        }
        normalizer
            .finish(|block| {
                encoder
                    .process(block, |frame| packets.push(frame.packet.to_vec()))
                    .expect("encode tail");
            })
            .expect("normalize tail");
        encoder.finish(|frame| packets.push(frame.packet.to_vec())).expect("encode tail");
        packets
    })
    .await
    .expect("player worker");

    assert_eq!(packets.len(), 5, "100 ms is five 20 ms packets");
    assert!(packets.iter().all(|packet| !packet.is_empty() && packet.len() <= MAX_PACKET_BYTES));
}
