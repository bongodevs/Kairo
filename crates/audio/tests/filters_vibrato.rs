use super::*;

#[test]
fn vibrato_configuration_enforces_the_protocol_bounds() {
    assert!(!VibratoConfig { depth: 0.0, ..VibratoConfig::default() }.is_enabled());
    assert!(matches!(
        Vibrato::new(VibratoConfig { frequency: 14.1, ..VibratoConfig::default() }),
        Err(FilterError::InvalidVibratoFrequency)
    ));
    assert!(matches!(
        Vibrato::new(VibratoConfig { depth: f32::NAN, ..VibratoConfig::default() }),
        Err(FilterError::InvalidVibratoDepth)
    ));
}

#[test]
fn vibrato_has_the_bounded_startup_delay_and_independent_channels() {
    let mut vibrato = Vibrato::new(VibratoConfig::default()).expect("vibrato");
    let mut samples = vec![0.0; 300 * 2];
    for frame in samples.as_chunks_mut::<2>().0 {
        frame[0] = 1.0;
        frame[1] = -1.0;
    }
    vibrato.process(&mut samples);

    assert!(samples[..20 * 2].iter().all(|sample| *sample == 0.0));
    for frame in samples[150 * 2..].as_chunks::<2>().0 {
        assert!((frame[0] - 1.0).abs() < 0.000_001);
        assert!((frame[1] + 1.0).abs() < 0.000_001);
    }
    assert!(std::mem::size_of::<Vibrato>() < 2_048);
}

#[test]
fn vibrato_delay_wrap_stays_in_bounds_over_multiple_lfo_cycles() {
    let mut vibrato = Vibrato::new(VibratoConfig::default()).expect("vibrato");
    let mut samples = vec![0.25; crate::SAMPLE_RATE as usize * 4];
    for block in samples.chunks_mut(crate::FRAME_SAMPLES * 2) {
        vibrato.process(block);
    }
    assert!(samples.iter().all(|sample| sample.is_finite()));
}

#[test]
fn vibrato_phase_stays_wrapped_and_finite_over_simulated_30_minutes() {
    let mut vibrato = Vibrato::new(VibratoConfig { frequency: 10.0, depth: 0.8 }).expect("vibrato");
    let mut block = [0.5_f32; 960 * 2];
    let total_frames = 48_000 * 60 * 30; // 30 minutes at 48kHz
    let mut frames_processed = 0;
    while frames_processed < total_frames {
        vibrato.process(&mut block);
        frames_processed += 960;
    }
    let phase = vibrato.phase();
    assert!(
        (0.0..std::f32::consts::TAU).contains(&phase),
        "Vibrato phase must stay in [0, TAU), got {phase}"
    );
    assert!(block.iter().all(|s| s.is_finite()));
}
