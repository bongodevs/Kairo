use super::*;
use crate::decode::{AudioFormat, Millis, PcmBlock};
fn format(sample_rate: u32, channels: usize) -> AudioFormat {
    AudioFormat { sample_rate, channels }
}

fn collect(
    normalizer: &mut PcmNormalizer,
    samples: &[f32],
    input_format: AudioFormat,
    block_frames: usize,
    position: Millis,
) -> Result<Vec<f32>, PcmError> {
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < samples.len() / input_format.channels {
        let frames = (samples.len() / input_format.channels - offset).min(block_frames);
        let start = offset * input_format.channels;
        let end = start + frames * input_format.channels;
        normalizer.process(
            PcmBlock { samples: &samples[start..end], format: input_format, frames, position },
            |block| output.extend_from_slice(block.samples),
        )?;
        offset += frames;
    }
    normalizer.finish(|block| output.extend_from_slice(block.samples))?;
    Ok(output)
}

#[test]
fn native_stereo_is_a_zero_copy_bypass() {
    let samples = [0.1, -0.1, 0.2, -0.2];
    let input_pointer = samples.as_ptr();
    let mut output_pointer = std::ptr::null();
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);

    normalizer
        .process(
            PcmBlock {
                samples: &samples,
                format: OUTPUT_FORMAT,
                frames: 2,
                position: Millis::new(50),
            },
            |block| {
                output_pointer = block.samples.as_ptr();
                assert_eq!(block.position, Millis::new(50));
            },
        )
        .expect("normalize");

    assert_eq!(output_pointer, input_pointer);
}

#[test]
fn mono_duplicates_and_multichannel_keeps_the_first_two() {
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
    let mono = [0.25, -0.5];
    let mono_output =
        collect(&mut normalizer, &mono, format(48_000, 1), 2, Millis::default()).expect("mono");
    assert_eq!(mono_output, [0.25, 0.25, -0.5, -0.5]);

    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
    let surround = [0.1, 0.2, 0.9, 1.0, -0.1, -0.2, -0.9, -1.0];
    let stereo = collect(&mut normalizer, &surround, format(48_000, 4), 2, Millis::default())
        .expect("multichannel");
    assert_eq!(stereo, [0.1, 0.2, -0.1, -0.2]);
}

#[test]
fn arbitrary_44100_blocks_finish_at_the_exact_48k_length() {
    let frames = 4_410;
    let mut input = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let value = (frame as f32 * 0.01).sin() * 0.5;
        input.extend_from_slice(&[value, -value]);
    }
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);

    let output = collect(&mut normalizer, &input, format(44_100, 2), 333, Millis::new(250))
        .expect("resample");

    assert_eq!(output.len(), 4_800 * 2);
    assert!(output.iter().all(|sample| sample.is_finite()));
}

#[test]
fn every_quality_drains_short_input_without_padding() {
    for quality in [ResamplingQuality::Low, ResamplingQuality::Medium, ResamplingQuality::High] {
        let input = vec![0.25; 441 * 2];
        let mut normalizer = PcmNormalizer::new(quality);
        let output = collect(&mut normalizer, &input, format(44_100, 2), 127, Millis::default())
            .expect("resample quality");
        assert_eq!(output.len(), 480 * 2, "{quality:?}");
    }
}

#[test]
fn reset_drops_buffered_pre_seek_audio_and_restarts_position() {
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
    let before = vec![1.0; 100 * 2];
    normalizer
        .process(
            PcmBlock {
                samples: &before,
                format: format(44_100, 2),
                frames: 100,
                position: Millis::default(),
            },
            |_| panic!("a partial chunk must stay buffered"),
        )
        .expect("buffer before seek");
    normalizer.reset(Millis::new(5_000));

    let after = vec![-0.25; 1_024 * 2];
    let mut output = Vec::new();
    let mut first_position = None;
    normalizer
        .process(
            PcmBlock {
                samples: &after,
                format: format(44_100, 2),
                frames: 1_024,
                position: Millis::new(5_000),
            },
            |block| {
                first_position.get_or_insert(block.position);
                output.extend_from_slice(block.samples);
            },
        )
        .expect("resample after seek");
    normalizer.finish(|block| output.extend_from_slice(block.samples)).expect("finish");

    assert_eq!(first_position, Some(Millis::new(5_000)));
    assert!(output.iter().all(|sample| *sample <= 0.0));
}

#[test]
fn output_storage_is_reused_across_resampler_calls() {
    let input = vec![0.25; 4_096 * 2];
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
    let mut addresses = Vec::new();
    normalizer
        .process(
            PcmBlock {
                samples: &input,
                format: format(44_100, 2),
                frames: 4_096,
                position: Millis::default(),
            },
            |block| addresses.push(block.samples.as_ptr() as usize),
        )
        .expect("resample");

    assert!(addresses.len() > 2);
    assert!(addresses[1..].iter().all(|address| *address == addresses[1]));
}

#[test]
fn output_positions_follow_one_frame_clock_without_block_drift() {
    let input = vec![0.25; 44_100 * 2];
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
    let mut blocks = Vec::new();
    for samples in input.chunks(777 * 2) {
        normalizer
            .process(
                PcmBlock {
                    samples,
                    format: format(44_100, 2),
                    frames: samples.len() / 2,
                    position: Millis::new(1_234),
                },
                |block| blocks.push((block.position, block.frames)),
            )
            .expect("resample");
    }
    normalizer.finish(|block| blocks.push((block.position, block.frames))).expect("finish");

    let mut emitted = 0u64;
    for (position, frames) in blocks {
        assert_eq!(position, Millis::new(1_234 + emitted * 1_000 / 48_000));
        emitted += u64::try_from(frames).expect("frame count");
    }
    assert_eq!(emitted, 48_000);
}

#[test]
fn malformed_blocks_and_absurd_rates_are_errors() {
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
    let error = normalizer
        .process(
            PcmBlock {
                samples: &[0.0],
                format: format(48_000, 2),
                frames: 1,
                position: Millis::default(),
            },
            |_| {},
        )
        .expect_err("bad shape");
    assert!(matches!(error, PcmError::InvalidBlock { .. }));

    let error = normalizer
        .process(
            PcmBlock {
                samples: &[0.0, 0.0],
                format: format(1, 2),
                frames: 1,
                position: Millis::default(),
            },
            |_| {},
        )
        .expect_err("bad rate");
    assert!(matches!(error, PcmError::UnsupportedSampleRate { .. }));

    let error = normalizer
        .process(
            PcmBlock {
                samples: &[],
                format: format(48_000, 0),
                frames: 1,
                position: Millis::default(),
            },
            |_| {},
        )
        .expect_err("zero channels");
    assert!(matches!(error, PcmError::InvalidChannels));
}
