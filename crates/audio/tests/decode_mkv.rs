use std::io::Cursor;

use kairo_core::{OpenedSource, SourceHints, SourceReference};
use tokio::runtime::Handle;

use super::read;
use crate::decode::{AudioFormat, BlockingDecoder, DecodeError};

const OPUS: &[u8] = include_bytes!("fixtures/opus-tail.mka");
const AAC: &[u8] = include_bytes!("fixtures/aac-tail.mka");
const OPUS_RATE: u32 = 48_000;
const OPUS_FRAMES: u64 = 48_000;
const AAC_RATE: u32 = 44_100;
const AAC_FRAMES: u64 = 45_056;

#[test]
fn the_audio_track_names_the_codec_delay_and_final_padding() {
    let mut input = Cursor::new(OPUS);
    let tracks = read(&mut input).expect("matroska tracks");
    assert_eq!(input.position(), 0, "the scan restores the stream position");
    let padding = tracks.track(1).expect("audio track");
    assert_eq!(padding.priming_frames(OPUS_RATE), 312);
    let time_base = symphonia::core::units::TimeBase::try_new(1, 1_000).expect("time base");
    let end = padding.end_trim(OPUS_RATE, time_base).expect("end trim");
    assert_eq!(end.expected_pts, 995);
    assert_eq!(end.frames, 648);
}

#[test]
fn a_track_without_discard_padding_has_no_end_trim() {
    let mut input = Cursor::new(AAC);
    let tracks = read(&mut input).expect("matroska tracks");
    let padding = tracks.track(1).expect("audio track");
    assert_eq!(padding.priming_frames(AAC_RATE), 1_024);
    let time_base = symphonia::core::units::TimeBase::try_new(1, 1_000).expect("time base");
    assert!(padding.end_trim(AAC_RATE, time_base).is_none());
}

#[test]
fn a_non_matroska_stream_has_no_padding() {
    let mut input = Cursor::new(b"not a matroska segment".to_vec());
    assert!(read(&mut input).is_none());
    assert_eq!(input.position(), 0, "the scan restores the stream position");
}

struct Decoded {
    format: AudioFormat,
    frames: u64,
    samples: usize,
}

#[tokio::test]
async fn an_opus_matroska_track_drops_its_codec_delay_and_discard_padding() {
    let decoded = decode(OPUS.to_vec(), "mka", "audio/x-matroska").await;
    assert_eq!(decoded.format.sample_rate, OPUS_RATE);
    assert_eq!(decoded.format.channels, 1);
    assert_eq!(decoded.frames, OPUS_FRAMES);
    assert_eq!(decoded.samples, OPUS_FRAMES as usize);
}

#[tokio::test]
async fn an_aac_matroska_track_drops_its_codec_delay() {
    let decoded = decode(AAC.to_vec(), "mka", "audio/x-matroska").await;
    assert_eq!(decoded.format.sample_rate, AAC_RATE);
    assert_eq!(decoded.format.channels, 1);
    assert_eq!(decoded.frames, AAC_FRAMES);
    assert_eq!(decoded.samples, AAC_FRAMES as usize);
}

async fn decode(bytes: Vec<u8>, extension: &str, mime: &str) -> Decoded {
    let runtime = Handle::current();
    let extension = extension.to_owned();
    let mime = mime.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(bytes, &extension, &mime), runtime)?;
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
    .expect("matroska decode")
}

fn source(bytes: Vec<u8>, extension: &str, mime: &str) -> OpenedSource {
    let length = bytes.len() as u64;
    OpenedSource::seekable(
        "memory",
        SourceReference::new(format!("padding.{extension}")),
        SourceHints {
            mime_type: Some(mime.to_owned()),
            file_extension: Some(extension.to_owned()),
        },
        Some(length),
        Cursor::new(bytes),
    )
}
