use std::io::Cursor;
use std::sync::Arc;

use tokio::runtime::Handle;

use kairo_core::{OpenedSource, SourceHints, SourceReference};

use super::*;
use crate::decode::BlockingDecoder;
use crate::test_wav as wav;
#[tokio::test]
async fn decoded_mono_44100_flows_to_stereo_48000_end_to_end() {
    let runtime = Handle::current();
    let bytes: Arc<[u8]> = wav::pcm_wave(44_100, 1, 4_410).into();
    let length = u64::try_from(bytes.len()).expect("fixture length");
    let output = tokio::task::spawn_blocking(move || {
        let source = OpenedSource::seekable(
            "memory",
            SourceReference::new("mono.wav"),
            SourceHints {
                mime_type: Some(String::from("audio/wav")),
                file_extension: Some(String::from("wav")),
            },
            Some(length),
            Cursor::new(bytes),
        );
        let mut decoder = BlockingDecoder::open(source, runtime).expect("open decoder");
        let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
        let mut output = Vec::new();
        while let Some(block) = decoder.next_block().expect("decode block") {
            normalizer
                .process(block, |block| output.extend_from_slice(block.samples))
                .expect("normalize block");
        }
        normalizer
            .finish(|block| output.extend_from_slice(block.samples))
            .expect("finish normalization");
        output
    })
    .await
    .expect("decode worker");

    assert_eq!(output.len(), 4_800 * 2);
    assert!(
        output.as_chunks::<2>().0.iter().all(|frame| (frame[0] - frame[1]).abs() < f32::EPSILON)
    );
}
