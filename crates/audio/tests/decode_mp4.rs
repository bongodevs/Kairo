use std::io::Cursor;

use kairo_core::{OpenedSource, SourceHints, SourceReference};
use tokio::runtime::Handle;

use super::read;
use crate::decode::{AudioFormat, BlockingDecoder, DecodeError};

const FIXTURE: &[u8] = include_bytes!("fixtures/aac-gapless.m4a");
const SAMPLE_RATE: u32 = 48_000;
const PRESENTED_FRAMES: u64 = 12_000;

#[test]
fn the_audio_edit_list_names_the_priming_and_presentation_length() {
    let mut input = Cursor::new(FIXTURE);
    let edit = read(&mut input).expect("edit list");
    let window = edit.window(SAMPLE_RATE).expect("presentation window");
    assert_eq!(window.priming, 1_024);
    assert_eq!(window.total, Some(PRESENTED_FRAMES));
    assert_eq!(input.position(), 0, "the scan restores the stream position");
}

struct Decoded {
    format: AudioFormat,
    frames: u64,
    samples: usize,
}

#[tokio::test]
async fn an_mp4_audio_track_is_trimmed_to_its_presentation_window() {
    let bytes = FIXTURE.to_vec();
    let runtime = Handle::current();
    let decoded = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(bytes), runtime)?;
        let mut format = None;
        let mut frames = 0u64;
        let mut samples = 0usize;
        while let Some(block) = decoder.next_block()? {
            format = Some(block.format);
            frames += block.frames as u64;
            samples += block.samples.len();
        }
        Ok::<_, DecodeError>(Decoded { format: format.expect("decoded format"), frames, samples })
    })
    .await
    .expect("decode worker")
    .expect("gapless decode");

    assert_eq!(decoded.format.sample_rate, SAMPLE_RATE);
    assert_eq!(decoded.format.channels, 2);
    assert_eq!(decoded.frames, PRESENTED_FRAMES);
    assert_eq!(decoded.samples, 2 * PRESENTED_FRAMES as usize);
}

fn source(bytes: Vec<u8>) -> OpenedSource {
    let length = bytes.len() as u64;
    OpenedSource::seekable(
        "memory",
        SourceReference::new("gapless.m4a"),
        SourceHints {
            mime_type: Some(String::from("audio/mp4")),
            file_extension: Some(String::from("m4a")),
        },
        Some(length),
        Cursor::new(bytes),
    )
}
