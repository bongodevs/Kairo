use super::*;

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
