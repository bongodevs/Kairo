use std::f64::consts::{SQRT_2, TAU};

use super::*;

const INPUT_RATE: u32 = 44_100;
const AMPLITUDE: f64 = 0.5;
const SECONDS: usize = 3;

#[derive(Debug)]
struct ToneResult {
    frequency: f64,
    gain_db: f64,
    thd_n_db: f64,
}

#[test]
#[ignore = "release-mode resampler fidelity measurement"]
fn high_quality_44k1_to_48k_meets_the_music_band_limits() {
    let mut results = Vec::new();
    for frequency in [20.0, 50.0, 100.0] {
        results.push(measure_tone(frequency));
    }
    for frequency in (200..=20_000).step_by(100) {
        results.push(measure_tone(f64::from(frequency)));
    }

    let minimum = results.iter().map(|result| result.gain_db).fold(f64::INFINITY, f64::min);
    let maximum = results.iter().map(|result| result.gain_db).fold(f64::NEG_INFINITY, f64::max);
    let ripple = maximum - minimum;
    let worst_thd_n = results
        .iter()
        .max_by(|left, right| left.thd_n_db.total_cmp(&right.thd_n_db))
        .expect("sweep result");

    let mut images = Vec::new();
    for frequency in (20_200..=22_000).step_by(100) {
        let frequency = f64::from(frequency);
        let output = resample_tone(frequency);
        let fundamental = magnitude(&output, frequency);
        let image_frequency = f64::from(INPUT_RATE) - frequency;
        let image = magnitude(&output, image_frequency);
        images.push((
            frequency,
            image_frequency,
            db(image / AMPLITUDE),
            db(image / fundamental),
            db(fundamental / AMPLITUDE),
        ));
    }
    let worst_image =
        images.iter().max_by(|left, right| left.2.total_cmp(&right.2)).expect("image result");
    let worst_passband_rejection = images
        .iter()
        .filter(|result| result.4 >= -1.0)
        .max_by(|left, right| left.3.total_cmp(&right.3))
        .expect("passband image result");

    println!(
        "HIGH 44.1->48 kHz: passband ripple={ripple:.6} dB (min={minimum:+.6}, max={maximum:+.6}); \
         worst THD+N={:.2} dB at {:.0} Hz; worst image={:.2} dBFS, {:.0} Hz -> {:.0} Hz; \
         worst passband image rejection={:.2} dBc at {:.0} Hz",
        worst_thd_n.thd_n_db,
        worst_thd_n.frequency,
        worst_image.2,
        worst_image.0,
        worst_image.1,
        worst_passband_rejection.3,
        worst_passband_rejection.0,
    );
    for frequency in [20.0, 1_000.0, 10_000.0, 15_000.0, 18_000.0, 20_000.0] {
        let result = results
            .iter()
            .find(|result| result.frequency == frequency)
            .expect("reported frequency");
        println!(
            "  {:>5.0} Hz: gain={:+.6} dB, THD+N={:.2} dB",
            result.frequency, result.gain_db, result.thd_n_db
        );
    }

    assert!(ripple <= 0.01, "passband ripple {ripple:.6} dB exceeds 0.01 dB");
    assert!(worst_image.2 <= -90.0, "image level {:.2} dBFS exceeds -90 dB", worst_image.2);
    assert!(
        worst_passband_rejection.3 <= -90.0,
        "image rejection {:.2} dBc exceeds -90 dB",
        worst_passband_rejection.3
    );
}

fn measure_tone(frequency: f64) -> ToneResult {
    let output = resample_tone(frequency);
    let amplitude = magnitude(&output, frequency);
    ToneResult {
        frequency,
        gain_db: db(amplitude / AMPLITUDE),
        thd_n_db: thd_n(&output, frequency),
    }
}

fn resample_tone(frequency: f64) -> Vec<f32> {
    let frames = INPUT_RATE as usize * SECONDS;
    let mut input = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let sample = (TAU * frequency * frame as f64 / f64::from(INPUT_RATE)).sin() * AMPLITUDE;
        input.extend_from_slice(&[sample as f32, sample as f32]);
    }

    let format = AudioFormat { sample_rate: INPUT_RATE, channels: CHANNELS };
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::High);
    let mut output = Vec::with_capacity(SAMPLE_RATE as usize * SECONDS * CHANNELS);
    let chunks = [37, 997, 128, 2_048];
    let mut offset = 0;
    let mut chunk = 0;
    while offset < frames {
        let count = chunks[chunk % chunks.len()].min(frames - offset);
        normalizer
            .process(
                PcmBlock {
                    samples: &input[offset * CHANNELS..(offset + count) * CHANNELS],
                    format,
                    frames: count,
                    position: Millis::default(),
                },
                |block| output.extend_from_slice(block.samples),
            )
            .expect("resample");
        offset += count;
        chunk += 1;
    }
    normalizer.finish(|block| output.extend_from_slice(block.samples)).expect("drain");

    assert_eq!(output.len(), SAMPLE_RATE as usize * SECONDS * CHANNELS);
    let start = SAMPLE_RATE as usize * CHANNELS;
    let end = start + SAMPLE_RATE as usize * CHANNELS;
    output[start..end].to_vec()
}

fn magnitude(samples: &[f32], frequency: f64) -> f64 {
    let frames = samples.len() / CHANNELS;
    let step = TAU * frequency / f64::from(SAMPLE_RATE);
    let (mut sine, mut cosine) = (0.0, 0.0);
    for frame in 0..frames {
        let angle = step * frame as f64;
        let sample = f64::from(samples[frame * CHANNELS]);
        sine += sample * angle.sin();
        cosine += sample * angle.cos();
    }
    2.0 * sine.hypot(cosine) / frames as f64
}

fn thd_n(samples: &[f32], frequency: f64) -> f64 {
    let frames = samples.len() / CHANNELS;
    let step = TAU * frequency / f64::from(SAMPLE_RATE);
    let (mut sine, mut cosine, mut dc) = (0.0, 0.0, 0.0);
    for frame in 0..frames {
        let angle = step * frame as f64;
        let sample = f64::from(samples[frame * CHANNELS]);
        sine += sample * angle.sin();
        cosine += sample * angle.cos();
        dc += sample;
    }
    let scale = 2.0 / frames as f64;
    let (sine, cosine, dc) = (sine * scale, cosine * scale, dc / frames as f64);
    let mut residual = 0.0;
    for frame in 0..frames {
        let angle = step * frame as f64;
        let fitted = sine * angle.sin() + cosine * angle.cos() + dc;
        let error = f64::from(samples[frame * CHANNELS]) - fitted;
        residual += error * error;
    }
    let noise_rms = (residual / frames as f64).sqrt();
    let signal_rms = sine.hypot(cosine) / SQRT_2;
    db(noise_rms / signal_rms)
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(f64::MIN_POSITIVE).log10()
}
