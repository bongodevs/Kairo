use super::*;

#[test]
fn protocol_volume_is_clamped_and_defaults_to_unity() {
    assert_eq!(Volume::default(), Volume::UNITY);
    assert_eq!(Volume::new(-1), Volume::SILENT);
    assert_eq!(Volume::new(1_001), Volume::MAX);
}

#[test]
fn unity_is_exact_silence_is_zero_and_amplification_clips() {
    let original = [-0.75, 0.25, 1.5];
    let mut unity = original;
    Volume::UNITY.apply(&mut unity);
    assert_eq!(unity, original);

    let mut silent = original;
    Volume::SILENT.apply(&mut silent);
    assert_eq!(silent, [0.0; 3]);

    let mut loud = [-0.5, 0.5];
    Volume::MAX.apply(&mut loud);
    assert_eq!(loud, [-1.0, i16::MAX as f32 / 32_768.0]);
}

#[test]
fn loudness_curve_keeps_the_two_reference_regions() {
    let mut lower = [0.1];
    Volume::new(150).apply(&mut lower);
    let expected_lower = (1.185_f32.tan() * 10_000.0) as i32 as f32 / 100_000.0;
    assert!((lower[0] - expected_lower).abs() < f32::EPSILON);

    let mut upper = [0.1];
    Volume::new(300).apply(&mut upper);
    assert!((upper[0] - 0.49242).abs() < 0.00001);
}

#[test]
fn live_changes_cross_fifty_frames_and_keep_passthrough_off_during_the_ramp() {
    let mut ramp = VolumeRamp::new(Volume::UNITY);
    ramp.set(Volume::SILENT);
    let mut sample = [1.0];
    let first = ramp.apply_frame(&mut sample);
    assert_eq!(first.level(), 98);
    for _ in 1..49 {
        sample[0] = 1.0;
        assert_ne!(ramp.apply_frame(&mut sample), Volume::SILENT);
    }
    sample[0] = 1.0;
    assert_eq!(ramp.apply_frame(&mut sample), Volume::SILENT);

    ramp.set(Volume::UNITY);
    assert_ne!(ramp.route_level(), Volume::UNITY.level());
    for _ in 0..50 {
        sample[0] = 1.0;
        ramp.apply_frame(&mut sample);
    }
    assert_eq!(ramp.route_level(), Volume::UNITY.level());
}
