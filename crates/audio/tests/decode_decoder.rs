use std::io::Cursor;
use std::sync::Arc;

use tokio::runtime::Handle;

use kairo_core::{OpenedSource, SourceHints, SourceReference};

use super::*;
use crate::test_wav as wav;
#[derive(Debug)]
struct Decoded {
    info: StreamInfo,
    samples: Vec<f32>,
    formats: Vec<AudioFormat>,
    positions: Vec<Millis>,
    buffer_addresses: Vec<usize>,
    stats: DecodeStats,
    eos_is_stable: bool,
}

async fn decode(bytes: Vec<u8>, seekable: bool) -> Result<Decoded, DecodeError> {
    let runtime = Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(bytes, seekable), runtime)?;
        let mut samples = Vec::new();
        let mut formats = Vec::new();
        let mut positions = Vec::new();
        let mut buffer_addresses = Vec::new();
        while let Some(block) = decoder.next_block()? {
            buffer_addresses.push(block.samples.as_ptr() as usize);
            samples.extend_from_slice(block.samples);
            formats.push(block.format);
            positions.push(block.position);
        }
        let eos_is_stable = decoder.next_block()?.is_none() && decoder.next_block()?.is_none();
        Ok(Decoded {
            info: decoder.info().clone(),
            samples,
            formats,
            positions,
            buffer_addresses,
            stats: decoder.stats(),
            eos_is_stable,
        })
    })
    .await
    .expect("decode worker")
}

fn source(bytes: Vec<u8>, seekable: bool) -> OpenedSource {
    let length = u64::try_from(bytes.len()).expect("fixture length");
    let reference = SourceReference::with_title("memory.wav", "fallback title");
    let hints = SourceHints {
        mime_type: Some(String::from("audio/wav")),
        file_extension: Some(String::from("wav")),
    };
    let reader = Cursor::new(Arc::<[u8]>::from(bytes));
    if seekable {
        OpenedSource::seekable("memory", reference, hints, Some(length), reader)
    } else {
        OpenedSource::read_only("memory", reference, hints, Some(length), reader)
    }
}

#[tokio::test]
async fn wav_decodes_to_reused_interleaved_f32_at_native_format() {
    let decoded = decode(wav::pcm_wave(48_000, 2, 1_000), true).await.expect("decode wav");

    assert_eq!(decoded.samples.len(), 2_000);
    assert!((decoded.samples[0] - 0.5).abs() < 0.0001);
    assert!((decoded.samples[1] + 0.5).abs() < 0.0001);
    assert!(
        decoded
            .samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|frame| { (frame[0] - 0.5).abs() < 0.0001 && (frame[1] + 0.5).abs() < 0.0001 })
    );
    assert!(
        decoded
            .formats
            .iter()
            .all(|format| { *format == AudioFormat { sample_rate: 48_000, channels: 2 } })
    );
    assert_eq!(decoded.info.native_format, decoded.formats.first().copied());
    assert_eq!(decoded.info.duration, Some(Millis::new(20)));
    assert_eq!(decoded.info.metadata.title.as_deref(), Some("fallback title"));
    assert!(decoded.stats.decoded_packets > 0);
    assert_eq!(decoded.stats.skipped_packets, 0);
    assert!(decoded.eos_is_stable);
    assert!(decoded.positions.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[tokio::test]
async fn decoder_leaves_mono_44100_for_the_resampler() {
    let decoded = decode(wav::pcm_wave(44_100, 1, 882), true).await.expect("decode wav");

    assert_eq!(decoded.samples.len(), 882);
    assert!(
        decoded
            .formats
            .iter()
            .all(|format| { *format == AudioFormat { sample_rate: 44_100, channels: 1 } })
    );
}

#[tokio::test]
async fn decoded_output_reuses_one_allocation_across_packets() {
    let decoded = decode(wav::pcm_wave(48_000, 2, 10_000), true).await.expect("decode wav");

    assert!(decoded.buffer_addresses.len() > 1);
    assert!(decoded.buffer_addresses.iter().all(|address| *address == decoded.buffer_addresses[0]));
}

#[tokio::test]
async fn seek_trims_to_the_first_sample_at_or_after_the_requested_time() {
    let runtime = Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        let mut decoder =
            BlockingDecoder::open(source(wav::pcm_wave(44_100, 1, 44_100), true), runtime)?;
        assert!(decoder.seek(Millis::new(123))?);
        let mut first_position = None;
        let mut frames = 0;
        while let Some(block) = decoder.next_block()? {
            first_position.get_or_insert(block.position);
            frames += block.frames;
        }
        Ok::<_, DecodeError>((first_position, frames))
    })
    .await
    .expect("decode worker")
    .expect("seek and decode");

    assert_eq!(result.0, Some(Millis::new(123)));
    assert_eq!(result.1, 44_100 - 5_425);
}

#[tokio::test]
async fn forward_only_sources_use_the_bounded_probe_buffer() {
    let decoded =
        decode(wav::pcm_wave(48_000, 2, 960), false).await.expect("decode forward-only wav");

    assert_eq!(decoded.samples.len(), 1_920);
    assert!(!decoded.info.seekable);
    assert_eq!(decoded.info.duration, Some(Millis::new(20)));
}

#[tokio::test]
async fn malformed_input_is_a_typed_probe_error() {
    let error = decode(b"not media".to_vec(), true).await.expect_err("invalid input rejected");
    assert!(matches!(error, DecodeError::Probe { .. }));
}

#[tokio::test]
async fn adts_is_registered_as_a_native_aac_container() {
    let bytes = crate::test_mpegts::adts_stream();
    let length = bytes.len() as u64;
    let source = OpenedSource::read_only(
        "memory",
        SourceReference::new("stream.aac"),
        SourceHints {
            mime_type: Some(String::from("audio/aac")),
            file_extension: Some(String::from("aac")),
        },
        Some(length),
        Cursor::new(bytes),
    );
    let runtime = Handle::current();
    let (info, frames) = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source, runtime)?;
        let info = decoder.info().clone();
        let mut frames = 0;
        while let Some(block) = decoder.next_block()? {
            frames += block.frames;
        }
        Ok::<_, DecodeError>((info, frames))
    })
    .await
    .expect("decode worker")
    .expect("ADTS probe");

    assert_eq!(info.container, "aac");
    assert_eq!(info.codec, "aac");
    assert_eq!(info.native_format, Some(AudioFormat::new(48_000, 2).expect("format")));
    assert!(frames > 0);
}
