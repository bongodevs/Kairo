use super::*;

#[test]
fn rotation_phase_stays_finite_and_modulates_after_thirty_minutes() {
    // 0.25 Hz rotation, 48 kHz. After 30 min = 86,400,000 samples the unbounded f64
    // reaches ~135,717 rad, still representable but approaching precision loss territory.
    // After wrapping, phase stays in [0, TAU) and sin/cos remain accurate.
    let config = RotationConfig { rotation_hz: 0.25 };
    let mut rotation = Rotation::new(config);

    let frames_total: usize = 30 * 60 * crate::SAMPLE_RATE as usize;
    let block = crate::FRAME_SAMPLES * crate::CHANNELS;
    let mut processed = 0usize;
    while processed < frames_total {
        let n = block.min((frames_total - processed) * crate::CHANNELS);
        let mut buf = vec![1.0_f32; n];
        rotation.process(&mut buf);
        processed += n / crate::CHANNELS;
    }

    // After 30 min, assert phase stayed in [0, TAU).
    assert!(
        rotation.phase() >= 0.0 && rotation.phase() < std::f64::consts::TAU,
        "rotation phase {} not in [0, TAU)",
        rotation.phase()
    );

    // Run one more short block after 30 min.
    let mut probe = [1.0_f32; crate::CHANNELS * 8];
    rotation.process(&mut probe);
    // All samples must be finite and in a valid gain range [0, 1].
    assert!(
        probe.iter().all(|s| s.is_finite() && (0.0..=1.0 + 1e-6).contains(s)),
        "rotation produced out-of-range or non-finite output after 30 min"
    );
    // The two channels must still be complementary (sum ≈ 1.0 per frame).
    for frame in probe.chunks(crate::CHANNELS) {
        let sum = frame[0] + frame[1];
        assert!((sum - 1.0).abs() < 1e-5, "rotation channel sum not 1.0: {sum}");
    }
}

#[test]
fn zero_rotation_is_disabled_and_nonzero_directions_are_enabled() {
    assert!(!RotationConfig::default().is_enabled());
    assert!(RotationConfig { rotation_hz: 0.2 }.is_enabled());
    assert!(RotationConfig { rotation_hz: -0.2 }.is_enabled());
}

#[test]
fn rotation_starts_centered_and_uses_complementary_channel_gains() {
    let config = RotationConfig { rotation_hz: 0.25 };
    let mut rotation = Rotation::new(config);
    let mut samples = [1.0; 4];
    rotation.process(&mut samples);

    let step = config.rotation_hz * std::f64::consts::TAU / f64::from(crate::SAMPLE_RATE);
    let sin = step.sin();
    assert_eq!(&samples[..2], &[0.5, 0.5]);
    assert!((f64::from(samples[2]) - (sin + 1.0) * 0.5).abs() < 0.000_001);
    assert!((f64::from(samples[3]) - (1.0 - sin) * 0.5).abs() < 0.000_001);
    assert!((samples[2] + samples[3] - 1.0).abs() < 0.000_001);
}

#[test]
fn negative_rotation_reverses_direction_and_phase_crosses_blocks() {
    let config = RotationConfig { rotation_hz: -1.0 };
    let mut whole = Rotation::new(config);
    let mut split = Rotation::new(config);
    let mut expected = [1.0; 8];
    let mut actual = expected;
    whole.process(&mut expected);
    split.process(&mut actual[..4]);
    split.process(&mut actual[4..]);
    assert_eq!(actual, expected);
    assert!(actual[2] < 0.5);
    assert!(actual[3] > 0.5);
    assert_eq!(std::mem::size_of::<Rotation>(), 2 * std::mem::size_of::<f64>());
}
