use super::*;

#[test]
fn cutoff_low_pass_first_impulse_matches_the_biquad_coefficient() {
    let config = PassConfig { cutoff_hz: 1_000, boost: 1.0 };
    let omega = std::f64::consts::TAU * 1_000.0 / f64::from(crate::SAMPLE_RATE);
    let alpha = omega.sin() / (2.0 * 0.707);
    let scale = 1.0 + alpha;
    let mut low = CutoffLowPass::new(config);
    let mut samples = [1.0, 0.0];
    low.process(&mut samples);
    let b0 = (1.0 - omega.cos()) * 0.5 / scale;
    assert!((f64::from(samples[0]) - b0).abs() < 0.000_001);
    assert_eq!(samples[1], 0.0);
}
