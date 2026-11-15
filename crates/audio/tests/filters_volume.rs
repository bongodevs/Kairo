use super::*;

#[test]
fn filter_volume_enforces_the_wire_range() {
    assert_eq!(Filters::default().volume, 1.0);
    for volume in [f32::NAN, f32::INFINITY, -0.1, 5.1] {
        assert!(matches!(FilterVolume::new(volume), Err(FilterError::InvalidFilterVolume)));
    }
    assert!(FilterVolume::new(0.0).is_ok());
    assert!(FilterVolume::new(5.0).is_ok());
}

#[test]
fn filter_volume_keeps_the_two_gain_regions_and_clips() {
    let mut lower = [0.25, -0.5];
    FilterVolume::new(0.5).expect("lower curve").process(&mut lower);
    let lower_multiplier = (f64::from(0.5_f32 * 0.79).tan() * 10_000.0) as f32;
    assert!((lower[0] - 0.25 * lower_multiplier / 10_000.0).abs() < f32::EPSILON);
    assert!((lower[1] + 0.5 * lower_multiplier / 10_000.0).abs() < f32::EPSILON);

    let mut upper = [0.25, -0.5];
    FilterVolume::new(2.0).expect("upper curve").process(&mut upper);
    let upper_multiplier = 24_612.0_f32 * 200.0 / 150.0;
    assert!((upper[0] - 0.25 * upper_multiplier / 10_000.0).abs() < f32::EPSILON);
    assert_eq!(upper[1], -1.0);
}

#[test]
fn near_unity_filter_volume_is_an_exact_noop() {
    let original = [0.25, -0.5, 1.25];
    let mut samples = original;
    FilterVolume::new(1.01).expect("near-unity volume").process(&mut samples);
    assert_eq!(samples, original);
}

#[test]
fn aggregate_validation_checks_every_fallible_active_filter() {
    assert!(matches!(
        Filters { volume: -0.1, ..Filters::default() }.validate(),
        Err(FilterError::InvalidFilterVolume)
    ));
    assert!(matches!(
        Filters {
            tremolo: Some(TremoloConfig { frequency: 0.0, ..TremoloConfig::default() }),
            ..Filters::default()
        }
        .validate(),
        Err(FilterError::InvalidTremoloFrequency)
    ));
    assert!(matches!(
        Filters {
            vibrato: Some(VibratoConfig { frequency: 15.0, ..VibratoConfig::default() }),
            ..Filters::default()
        }
        .validate(),
        Err(FilterError::InvalidVibratoFrequency)
    ));
    assert!(matches!(
        Filters {
            echo: Some(EchoConfig { length_seconds: MAX_ECHO_SECONDS + 0.1, decay: 0.5 }),
            ..Filters::default()
        }
        .validate(),
        Err(FilterError::EchoTooLong { .. })
    ));
}

#[test]
fn disabled_filter_configs_remain_valid_state() {
    let filters = Filters {
        tremolo: Some(TremoloConfig { frequency: 0.0, depth: 0.0 }),
        vibrato: Some(VibratoConfig { frequency: 15.0, depth: 0.0 }),
        echo: Some(EchoConfig::default()),
        ..Filters::default()
    };
    filters.validate().expect("disabled filters");
}
