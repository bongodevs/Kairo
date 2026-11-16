use player::decode::{AudioBlock, BlockingDecoder, Millis};
use player::opus::OpusRoute;
use player::source::{SourceMetadata, SourceReference};

use super::*;

fn reference() -> SourceReference {
    SourceReference::new("song-id")
}

fn metadata() -> SourceMetadata {
    SourceMetadata {
        title: Some(String::from("Song")),
        author: Some(String::from("Artist")),
        ..SourceMetadata::default()
    }
}

#[test]
fn opened_source_keeps_the_track_identity_and_metadata() {
    let reference = reference();
    let metadata = metadata();
    let opened = opened_source("jiosaavn", &reference, &metadata);

    assert_eq!(opened.source_name(), "jiosaavn");
    assert_eq!(opened.reference(), &reference);
    assert_eq!(opened.metadata(), &metadata);
    assert!(opened.is_seekable());
    assert_eq!(opened.hints().mime_type.as_deref(), Some("audio/ogg"));
    assert_eq!(opened.hints().file_extension.as_deref(), Some("ogg"));
    assert!(opened.content_length().is_some_and(|length| length > 0));
}

#[test]
fn one_marker_is_taken_per_placeholder_open() {
    let log = PlaceholderLog::default();
    let reference = reference();

    log.record("jiosaavn", &reference);
    log.record("jiosaavn", &reference);
    assert!(log.take("jiosaavn", &reference));
    assert!(log.take("jiosaavn", &reference));
    assert!(!log.take("jiosaavn", &reference));
    assert!(!log.take("youtube", &reference));
}

#[tokio::test]
async fn generated_stream_decodes_thirty_seconds_of_silence() {
    let source = opened_source("jiosaavn", &reference(), &metadata());
    let runtime = tokio::runtime::Handle::current();
    let (frames, peak) = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source, runtime).expect("open placeholder");
        assert_eq!(decoder.info().codec, "opus");
        assert_eq!(decoder.info().duration, Some(Millis::new(30_000)));
        let mut frames = 0u64;
        let mut peak = 0.0f32;
        while let Some(block) = decoder
            .next_audio(OpusRoute { volume: 100, filters_active: true })
            .expect("decode placeholder")
        {
            match block {
                AudioBlock::Pcm(block) => {
                    frames += u64::try_from(block.frames).expect("frame count");
                    peak = peak.max(
                        block.samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs())),
                    );
                }
                AudioBlock::Opus(block) => {
                    frames += u64::try_from(block.info.total_samples).expect("sample count");
                }
            }
        }
        (frames, peak)
    })
    .await
    .expect("worker");

    assert_eq!(frames, 30 * 50 * 960);
    assert!(peak < 1e-12, "placeholder is audible: peak {peak}");
}
