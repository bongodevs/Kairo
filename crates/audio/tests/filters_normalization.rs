use super::*;

#[test]
fn normalization_preserves_stereo_channel_ratio() {
    // Left channel peak 0.9, right channel peak 0.1.
    // One global peak (0.9) → one gain applied to both channels → ratio preserved.
    let max = 0.5;
    let mut filter =
        Normalization::new(NormalizationConfig { max_amplitude: max, adaptive: false });
    // Stereo interleaved: [L, R, L, R, ...].
    let mut samples = vec![0.9_f32, 0.1_f32, 0.9, 0.1, 0.9, 0.1, 0.9, 0.1];
    filter.process(&mut samples);

    let left = samples[0];
    let right = samples[1];
    let expected_gain = max / 0.9;
    assert!((left - 0.9 * expected_gain).abs() < 1e-5, "left channel after normalization: {left}");
    assert!(
        (right - 0.1 * expected_gain).abs() < 1e-5,
        "right channel after normalization: {right}"
    );
    let ratio = left / right;
    assert!((ratio - 9.0).abs() < 0.01, "channel ratio broken: {ratio}");
}

#[test]
fn normalization_clamps_when_right_channel_is_louder() {
    // Right channel peak 0.9 > left channel peak 0.1.
    // The old per-channel loop reset self.peak=0 for channel 0, found peak=0.1 (< max=0.5),
    // applied no gain to channel 0, then for channel 1 still had peak=0.1 (not reset again),
    // found peak grows to 0.9 and applied gain=0.5/0.9. Channel 0 stayed at 0.1 raw.
    // With a single global peak the gain 0.5/0.9 is applied to BOTH channels.
    let max = 0.5;
    let mut filter =
        Normalization::new(NormalizationConfig { max_amplitude: max, adaptive: false });
    // Left=0.1, Right=0.9 (right is loud).
    let mut samples = vec![0.1_f32, 0.9_f32, 0.1, 0.9, 0.1, 0.9, 0.1, 0.9];
    filter.process(&mut samples);

    let left = samples[0];
    let right = samples[1];
    let expected_gain = max / 0.9;
    // Both channels must be scaled by the same global gain.
    assert!(
        (left - 0.1 * expected_gain).abs() < 1e-5,
        "left channel should be scaled by global gain, got: {left}"
    );
    assert!(
        (right - 0.9 * expected_gain).abs() < 1e-5,
        "right channel after normalization: {right}"
    );
    // Ratio preserved.
    let ratio = right / left;
    assert!((ratio - 9.0).abs() < 0.01, "channel ratio broken: {ratio}");
}

#[test]
fn nonpositive_maximum_disables_normalization() {
    assert!(!NormalizationConfig::default().is_enabled());
    assert!(!NormalizationConfig { max_amplitude: -1.0, adaptive: true }.is_enabled());
    assert!(NormalizationConfig { max_amplitude: 0.5, adaptive: true }.is_enabled());
}

#[test]
fn normalization_applies_single_gain_across_channels() {
    let mut filter =
        Normalization::new(NormalizationConfig { max_amplitude: 0.5, adaptive: false });
    let mut samples = [0.4, 1.0];
    filter.process(&mut samples);
    assert!((samples[0] - 0.2).abs() < 1e-6);
    assert!((samples[1] - 0.5).abs() < 1e-6);
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
