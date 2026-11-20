use super::*;

/// Tremolo at 2 Hz, 48 kHz: step ≈ 2.618e-4 rad/sample.
/// After 30 min × 48000 samples/s = 86,400,000 samples the unbounded f32 accumulator
/// reaches ~22,619 rad, which is a large but still-representable f32. However the purpose
/// of the wrap is to prevent the accumulator from growing without bound over very long
/// streams and to prove the output is still periodic, not constant or NaN.
#[test]
fn tremolo_phase_stays_in_tau_and_keeps_modulating_after_thirty_minutes() {
    // Pull the phase field out via a reflective approach: run the filter and check
    // that output is still non-trivially modulating (varies across frames).
    let config = TremoloConfig { frequency: 2.0, depth: 1.0 };
    let mut tremolo = Tremolo::new(config).expect("tremolo");

    // 30 min of samples at 48 kHz, fed in FRAME_SAMPLES-sized blocks (960 stereo = 1920 f32).
    let frames_total: usize = 30 * 60 * crate::SAMPLE_RATE as usize;
    let block = crate::FRAME_SAMPLES * crate::CHANNELS;
    let mut processed = 0usize;
    while processed < frames_total {
        let n = block.min((frames_total - processed) * crate::CHANNELS);
        let mut buf = vec![1.0_f32; n];
        tremolo.process(&mut buf);
        processed += n / crate::CHANNELS;
    }

    // After 30 min, assert phase stayed in [0, TAU).
    assert!(
        tremolo.phase() >= 0.0 && tremolo.phase() < std::f32::consts::TAU,
        "tremolo phase {} not in [0, TAU)",
        tremolo.phase()
    );

    // Run one more short block and confirm output still varies (not flat-lined).
    let mut probe = [1.0_f32; crate::CHANNELS * 8]; // 8 frames
    tremolo.process(&mut probe);
    // With depth=1.0, gain = 0.5 + 0.5*sin(phase). It oscillates 0..1, never constant.
    let first = probe[0];
    let varying = probe.chunks(crate::CHANNELS).any(|f| (f[0] - first).abs() > 1e-6);
    assert!(varying, "tremolo output became constant after 30 min — phase likely NaN or stuck");
    // All samples must be finite.
    assert!(probe.iter().all(|s| s.is_finite()), "tremolo produced non-finite output");
}

#[test]
fn tremolo_configuration_enforces_the_protocol_bounds() {
    assert!(!TremoloConfig { depth: 0.0, ..TremoloConfig::default() }.is_enabled());
    assert!(matches!(
        Tremolo::new(TremoloConfig { frequency: 0.0, ..TremoloConfig::default() }),
        Err(FilterError::InvalidTremoloFrequency)
    ));
    assert!(matches!(
        Tremolo::new(TremoloConfig { depth: 1.1, ..TremoloConfig::default() }),
        Err(FilterError::InvalidTremoloDepth)
    ));
}

#[test]
fn tremolo_starts_at_the_reference_gain_and_preserves_stereo_balance() {
    let config = TremoloConfig::default();
    let mut tremolo = Tremolo::new(config).expect("tremolo");
    let mut samples = [1.0, -1.0, 1.0, -1.0];
    tremolo.process(&mut samples);

    let depth = config.depth * 0.5;
    let first = 1.0 - depth;
    let phase = std::f32::consts::TAU * config.frequency / crate::SAMPLE_RATE as f32;
    let second = first + depth * phase.sin();
    assert_eq!(samples[0], first);
    assert_eq!(samples[1], -first);
    assert!((samples[2] - second).abs() < 0.000_001);
    assert_eq!(samples[2], -samples[3]);
}

#[test]
fn tremolo_phase_is_continuous_across_input_blocks() {
    let mut whole = Tremolo::new(TremoloConfig::default()).expect("tremolo");
    let mut split = Tremolo::new(TremoloConfig::default()).expect("tremolo");
    let mut expected = [0.5; 8];
    let mut actual = expected;
    whole.process(&mut expected);
    split.process(&mut actual[..4]);
    split.process(&mut actual[4..]);
    assert_eq!(actual, expected);
    assert!(std::mem::size_of::<Tremolo>() < 32);
}
