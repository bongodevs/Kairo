use super::*;
use crate::{FRAME_SAMPLES, SAMPLE_RATE};

fn process(samples: &[f32]) -> (Vec<f32>, Limiter) {
    let mut output = samples.to_vec();
    let mut limiter = Limiter::new();
    limiter.process(&mut output);
    (output, limiter)
}

#[test]
fn unity_material_below_activation_keeps_every_bit() {
    let input = [0.0, -0.0, 0.25, -0.5, THRESHOLD, -THRESHOLD, 0.123_456_79, -0.939_9];
    let (output, limiter) = process(&input);
    for (before, after) in input.iter().zip(output) {
        assert_eq!(before.to_bits(), after.to_bits());
    }
    assert_eq!(limiter.gain(), 1.0);
}

#[test]
fn hot_clusters_stay_finite_and_below_full_scale() {
    let input = [0.96, -0.99, 1.05, -1.2, 1.6, -2.0, 0.5, -0.97];
    let (output, limiter) = process(&input);
    assert!(output.iter().all(|sample| sample.is_finite() && sample.abs() < 1.0));
    assert!(limiter.gain() < 1.0);
}

#[test]
fn two_loud_tracks_can_crossfade_without_clipping() {
    let input = vec![2.0f32; SAMPLE_RATE as usize * CHANNELS];
    let (output, _) = process(&input);
    assert!(output.iter().all(|sample| sample.abs() < 1.0));
}

#[test]
fn silence_stays_silent_and_releases_exactly_to_unity() {
    let mut limiter = Limiter::new();
    let mut hot = vec![1.5; FRAME_SAMPLES * CHANNELS];
    limiter.process(&mut hot);
    assert!(limiter.gain() < 1.0);

    let mut silence = vec![0.0; SAMPLE_RATE as usize * 2 * CHANNELS];
    limiter.process(&mut silence);
    assert!(silence.iter().all(|sample| *sample == 0.0));
    assert_eq!(limiter.gain(), 1.0);
}

#[test]
fn non_finite_and_extreme_input_is_guarded_in_the_same_pass() {
    let input = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, -f32::MAX, 0.25];
    let (output, _) = process(&input);
    assert_eq!(&output[..3], &[0.0, 0.0, 0.0]);
    assert!(output.iter().all(|sample| sample.is_finite() && (-1.0..=1.0).contains(sample)));
}

#[test]
fn the_knee_is_continuous_with_unit_slope() {
    assert_eq!(soft_knee(THRESHOLD), THRESHOLD);
    assert_eq!(soft_knee(-THRESHOLD), -THRESHOLD);
    let step = 0.000_1;
    let slope = (soft_knee(THRESHOLD + step) - THRESHOLD) / step;
    assert!((slope - 1.0).abs() < 0.01, "knee slope was {slope}");
    assert!(soft_knee(f32::MAX) <= CEILING);
}

#[test]
#[ignore = "release-mode limiter cost measurement"]
fn measure_one_frame_cost() {
    let mut limiter = Limiter::new();
    let mut frame = (0..FRAME_SAMPLES * CHANNELS)
        .map(|index| if index % 97 == 0 { 1.1 } else { 0.4 })
        .collect::<Vec<_>>();
    let iterations = 100_000;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        limiter.process(&mut frame);
        std::hint::black_box(&frame);
    }
    let per_frame = start.elapsed() / iterations;
    println!(
        "limiter: {per_frame:?}/frame ({:.4}% of one core)",
        per_frame.as_secs_f64() / 0.020 * 100.0
    );
}
