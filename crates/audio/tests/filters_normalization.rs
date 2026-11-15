use super::*;

#[test]
fn nonpositive_maximum_disables_normalization() {
    assert!(!NormalizationConfig::default().is_enabled());
    assert!(!NormalizationConfig { max_amplitude: -1.0, adaptive: true }.is_enabled());
    assert!(NormalizationConfig { max_amplitude: 0.5, adaptive: true }.is_enabled());
}

#[test]
fn normalization_preserves_channel_major_peak_updates() {
    let mut filter =
        Normalization::new(NormalizationConfig { max_amplitude: 0.5, adaptive: false });
    let mut samples = [0.4, 1.0];
    filter.process(&mut samples);
    assert_eq!(samples[0], 0.4);
    assert_eq!(samples[1], 0.5);
}

#[test]
fn adaptive_peak_persists_while_frame_local_peak_resets() {
    let config = NormalizationConfig { max_amplitude: 0.5, adaptive: true };
    let mut adaptive = Normalization::new(config);
    let mut loud = [1.0; 2];
    adaptive.process(&mut loud);
    let mut quiet = [0.25; 2];
    adaptive.process(&mut quiet);
    assert_eq!(quiet, [0.125, 0.125]);

    let mut local = Normalization::new(NormalizationConfig { adaptive: false, ..config });
    local.process(&mut loud);
    let mut quiet = [0.25; 2];
    local.process(&mut quiet);
    assert_eq!(quiet, [0.25, 0.25]);
}

#[test]
fn maximum_is_clamped_to_the_normalized_pcm_range() {
    let mut filter =
        Normalization::new(NormalizationConfig { max_amplitude: 2.0, adaptive: false });
    let mut samples = [2.0, -2.0];
    filter.process(&mut samples);
    assert_eq!(samples, [1.0, -1.0]);
    assert!(std::mem::size_of::<Normalization>() < 16);
}
