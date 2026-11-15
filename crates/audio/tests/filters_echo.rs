use super::*;

#[test]
fn echo_requires_a_usable_length_and_positive_decay() {
    assert!(!EchoConfig::default().is_enabled());
    assert!(matches!(
        Echo::new(EchoConfig { length_seconds: 0.5 / crate::SAMPLE_RATE as f32, decay: 0.5 }),
        Err(FilterError::InvalidEchoLength)
    ));
    assert!(matches!(
        Echo::new(EchoConfig { length_seconds: MAX_ECHO_SECONDS + 0.1, decay: 0.5 }),
        Err(FilterError::EchoTooLong { .. })
    ));
    assert!(matches!(
        Echo::new(EchoConfig { length_seconds: 0.1, decay: 0.0 }),
        Err(FilterError::InvalidEchoDecay)
    ));
}

#[test]
fn echo_repeats_the_feedback_at_exact_delay_intervals() {
    let mut echo =
        Echo::new(EchoConfig { length_seconds: 4.5 / crate::SAMPLE_RATE as f32, decay: 0.5 })
            .expect("echo");
    let mut samples = vec![0.0; 9 * 2];
    samples[..2].copy_from_slice(&[1.0, -1.0]);
    echo.process(&mut samples);
    assert_eq!(&samples[..2], &[1.0, -1.0]);
    assert_eq!(&samples[4 * 2..5 * 2], &[0.5, -0.5]);
    assert_eq!(&samples[8 * 2..9 * 2], &[0.25, -0.25]);
}

#[test]
fn echo_state_crosses_blocks_without_mixing_channels() {
    let config = EchoConfig { length_seconds: 3.5 / crate::SAMPLE_RATE as f32, decay: 0.4 };
    let mut whole = Echo::new(config).expect("echo");
    let mut split = Echo::new(config).expect("echo");
    let mut expected = [1.0, -0.5, 0.2, 0.0, -0.1, 0.4, 0.0, 0.0, 0.0, 0.0];
    let mut actual = expected;
    whole.process(&mut expected);
    split.process(&mut actual[..4]);
    split.process(&mut actual[4..]);
    assert_eq!(actual, expected);
    assert_ne!(actual[6], actual[7]);
}

#[test]
fn maximum_echo_length_has_the_documented_frame_bound() {
    let echo = Echo::new(EchoConfig { length_seconds: MAX_ECHO_SECONDS, decay: 0.5 })
        .expect("bounded echo");
    assert_eq!(echo.delay_frames(), (crate::SAMPLE_RATE as f32 * MAX_ECHO_SECONDS) as usize);
}
