use super::*;

#[test]
fn default_distortion_is_the_protocol_identity_gate() {
    assert!(!DistortionConfig::default().is_enabled());
    assert!(DistortionConfig { sin_offset: 0.1, ..DistortionConfig::default() }.is_enabled());
}

#[test]
fn distortion_matches_the_nonlinear_sample_formula() {
    let config = DistortionConfig {
        sin_offset: 0.1,
        sin_scale: 0.8,
        cos_offset: -0.2,
        cos_scale: 1.2,
        tan_offset: 0.05,
        tan_scale: 0.7,
        offset: -0.1,
        scale: 0.9,
    };
    let input = 0.25f32;
    let sin = config.sin_offset + (input * config.sin_scale).sin();
    let cos = config.cos_offset + (input * config.cos_scale).cos();
    let tan = config.tan_offset + (input * config.tan_scale).tan();
    let expected = (config.offset + config.scale * sin * cos * tan).clamp(-1.0, 1.0);
    let mut samples = [input, -input];
    Distortion::new(config).process(&mut samples);
    assert!((samples[0] - expected).abs() < 0.000_001);
    assert_ne!(samples[0], samples[1]);
}

#[test]
fn distortion_clamps_extreme_output_and_has_no_history() {
    let filter = Distortion::new(DistortionConfig { offset: 10.0, ..DistortionConfig::default() });
    let mut first = [0.2, -0.4];
    let mut second = first;
    filter.process(&mut first);
    filter.process(&mut second);
    assert_eq!(first, [1.0, 1.0]);
    assert_eq!(first, second);
    assert!(std::mem::size_of::<Distortion>() <= 8 * std::mem::size_of::<f32>());
}
