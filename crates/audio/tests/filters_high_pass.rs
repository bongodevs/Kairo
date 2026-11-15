use super::*;

#[test]
fn cutoff_zero_disables_the_pass_filters() {
    assert!(!PassConfig::default().is_enabled());
    assert!(PassConfig { cutoff_hz: 1, ..PassConfig::default() }.is_enabled());
}

#[test]
fn high_pass_first_impulse_matches_the_biquad_coefficient() {
    let config = PassConfig { cutoff_hz: 1_000, boost: 1.0 };
    let omega = std::f64::consts::TAU * 1_000.0 / f64::from(crate::SAMPLE_RATE);
    let alpha = omega.sin() / (2.0 * 0.707);
    let scale = 1.0 + alpha;
    let mut high = HighPass::new(config);
    let mut samples = [1.0, 0.0];
    high.process(&mut samples);
    let b0 = (1.0 + omega.cos()) * 0.5 / scale;
    assert!((f64::from(samples[0]) - b0).abs() < 0.000_001);
    assert_eq!(samples[1], 0.0);
}

#[test]
fn high_pass_state_is_streaming_and_channel_independent() {
    let config = PassConfig { cutoff_hz: 2_000, boost: 1.0 };
    let mut whole = HighPass::new(config);
    let mut split = HighPass::new(config);
    let mut expected = [1.0, -0.5, 0.2, 0.0, -0.1, 0.4, 0.0, 0.0];
    let mut actual = expected;
    whole.process(&mut expected);
    split.process(&mut actual[..4]);
    split.process(&mut actual[4..]);
    assert_eq!(actual, expected);
    assert_ne!(actual[0], actual[1]);
}

#[test]
fn high_pass_boost_is_clamped_at_the_output_boundary() {
    let mut high = HighPass::new(PassConfig { cutoff_hz: 1_000, boost: 100.0 });
    let mut samples = [1.0, -1.0];
    high.process(&mut samples);
    assert_eq!(samples, [1.0, -1.0]);
}
