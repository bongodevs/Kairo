use super::*;

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
