use super::*;

#[test]
fn gains_clamp_and_invalid_bands_are_ignored() {
    let mut config = EqualizerConfig::new();
    config.set_gain(0, -1.0);
    config.set_gain(14, 2.0);
    config.set_gain(15, 0.5);
    assert_eq!(config.gain(0), -0.25);
    assert_eq!(config.gain(14), 1.0);
    assert_eq!(config.gain(15), 0.0);
    assert!(config.is_enabled());
}

#[test]
fn disabled_equalizer_is_bit_exact() {
    let mut equalizer = Equalizer::new(EqualizerConfig::default());
    let mut samples = [0.25, -0.5, -0.75, 1.0];
    let original = samples;
    equalizer.process(&mut samples);
    assert_eq!(samples, original);
}

#[test]
fn first_band_impulse_matches_the_fixed_recurrence_and_channels_are_independent() {
    let config = EqualizerConfig::from_bands([(0, 1.0)]);
    let mut equalizer = Equalizer::new(config);
    let mut samples = [0.1, 0.0, 0.0, 0.0];
    equalizer.process(&mut samples);

    let first_band = 7.622_667e-4_f32 * 0.1;
    assert!((samples[0] - (0.025 + first_band) * 4.0).abs() < 0.000_001);
    assert_eq!(samples[1], 0.0);
    assert!((samples[2] - first_band * 1.998_464_8 * 4.0).abs() < 0.000_001);
    assert_eq!(samples[3], 0.0);
}

#[test]
fn replacing_configuration_resets_filter_history() {
    let config = EqualizerConfig::from_bands([(4, 0.5)]);
    let mut equalizer = Equalizer::new(config);
    let mut first = [0.4, -0.4];
    equalizer.process(&mut first);
    equalizer.set_config(config);
    let mut second = [0.4, -0.4];
    equalizer.process(&mut second);
    assert_eq!(first, second);
}

#[test]
fn processor_state_stays_below_one_kibibyte() {
    assert!(std::mem::size_of::<Equalizer>() < 1_024);
}

#[test]
fn karaoke_level_zero_is_identity() {
    let mut karaoke = Karaoke::new(KaraokeConfig { level: 0.0, ..KaraokeConfig::default() });
    let mut samples = [0.2, -0.4, -0.7, 0.9];
    let original = samples;
    karaoke.process(&mut samples);
    assert_eq!(samples, original);
}

#[test]
fn karaoke_first_sample_matches_the_center_filter() {
    let config = KaraokeConfig::default();
    let mut karaoke = Karaoke::new(config);
    let mut samples = [0.5, 0.5];
    karaoke.process(&mut samples);

    let rate = f64::from(crate::SAMPLE_RATE);
    let c = (-2.0 * std::f64::consts::PI * 100.0 / rate).exp();
    let b = -4.0 * c / (1.0 + c) * (2.0 * std::f64::consts::PI * 220.0 / rate).cos();
    let a = (1.0 - b * b / (4.0 * c)).sqrt() * (1.0 - c);
    assert!((samples[0] - a as f32 * 0.5).abs() < 0.000_001);
    assert_eq!(samples[0], samples[1]);
}

#[test]
fn karaoke_reset_drops_filter_history() {
    let mut karaoke = Karaoke::new(KaraokeConfig::default());
    let mut first = [0.4, -0.2];
    karaoke.process(&mut first);
    karaoke.reset();
    let mut second = [0.4, -0.2];
    karaoke.process(&mut second);
    assert_eq!(first, second);
}

#[test]
fn timescale_speed_and_rate_change_duration_while_pitch_preserves_it() {
    let normal = timescale_output(TimescaleConfig::default());
    let fast = timescale_output(TimescaleConfig { speed: 2.0, ..TimescaleConfig::default() });
    let pitched = timescale_output(TimescaleConfig { pitch: 2.0, ..TimescaleConfig::default() });
    let rated = timescale_output(TimescaleConfig { rate: 2.0, ..TimescaleConfig::default() });
    assert!(fast.len() < normal.len() * 3 / 4);
    assert!(rated.len() < normal.len() * 3 / 4);
    assert!(pitched.len() > normal.len() * 3 / 4);
    assert!(pitched.len() < normal.len() * 5 / 4);
    assert!(pitched.iter().all(|sample| sample.is_finite()));
    assert!((frequency(&normal) - 440.0).abs() < 20.0);
    assert!((frequency(&fast) - 440.0).abs() < 20.0);
    assert!((frequency(&pitched) - 880.0).abs() < 30.0);
    assert!((frequency(&rated) - 880.0).abs() < 30.0);
}

#[test]
fn timescale_factors_are_bounded_and_report_the_effective_source_speed() {
    let fast = TimescaleConfig { speed: 8.0, ..TimescaleConfig::default() };
    let combined = TimescaleConfig { speed: 4.0, pitch: 4.0, rate: 4.0 };
    let stopped = TimescaleConfig { speed: 0.0, ..TimescaleConfig::default() };
    let invalid = TimescaleConfig { speed: f64::NAN, ..TimescaleConfig::default() };
    let cancelling = TimescaleConfig { speed: 2.0, pitch: 2.0, rate: 0.5 };

    assert_eq!(fast.source_speed(), 4.0);
    assert_eq!(combined.source_speed(), 4.0);
    assert_eq!(stopped.source_speed(), 0.25);
    assert_eq!(invalid.source_speed(), 1.0);
    assert!(cancelling.is_enabled());
}

#[test]
fn timescale_rejects_unaligned_stereo_input() {
    let mut timescale =
        Timescale::new(TimescaleConfig { speed: 1.25, ..TimescaleConfig::default() })
            .expect("timescale");
    let error = timescale.process(&[0.0], |_| {}).expect_err("unaligned input");
    assert!(matches!(error, FilterError::UnalignedTimescaleInput { samples: 1 }));
}

#[test]
fn timescale_reset_drops_buffered_audio_and_restarts_deterministically() {
    let input = tone(9_600);
    let config = TimescaleConfig { speed: 1.25, ..TimescaleConfig::default() };
    let mut timescale = Timescale::new(config).expect("timescale");
    let first = process_timescale(&mut timescale, &input);
    timescale.reset();
    let second = process_timescale(&mut timescale, &input);
    assert!(!first.is_empty());
    assert_eq!(first, second);
}

fn timescale_output(config: TimescaleConfig) -> Vec<f32> {
    let input = tone(9_600);
    let mut output = Vec::new();
    let mut timescale = Timescale::new(config).expect("timescale");
    for chunk in input.chunks(960 * 2) {
        timescale.process(chunk, |samples| output.extend_from_slice(samples)).expect("process");
    }
    timescale.drain(|samples| output.extend_from_slice(samples)).expect("drain");
    output
}

fn tone(frames: usize) -> Vec<f32> {
    let mut input = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let sample =
            (frame as f32 * 440.0 * std::f32::consts::TAU / crate::SAMPLE_RATE as f32).sin() * 0.25;
        input.extend_from_slice(&[sample, -sample]);
    }
    input
}

fn process_timescale(timescale: &mut Timescale, input: &[f32]) -> Vec<f32> {
    let mut output = Vec::new();
    for chunk in input.chunks(960 * 2) {
        timescale.process(chunk, |samples| output.extend_from_slice(samples)).expect("process");
    }
    output
}

fn frequency(samples: &[f32]) -> f64 {
    let frames = samples.len() / 2;
    let start = frames / 5;
    let end = frames - start;
    let crossings = (start + 1..end)
        .filter(|frame| samples[(frame - 1) * 2] <= 0.0 && samples[frame * 2] > 0.0)
        .count();
    crossings as f64 * f64::from(crate::SAMPLE_RATE) / (end - start) as f64
}
