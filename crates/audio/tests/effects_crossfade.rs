use super::{CrossfadeCurve, CrossfadeOptions};

#[test]
fn curves_hit_endpoints_and_clamp() {
    for curve in [
        CrossfadeCurve::Linear,
        CrossfadeCurve::Exponential,
        CrossfadeCurve::Logarithmic,
        CrossfadeCurve::SCurve,
        CrossfadeCurve::Sinusoidal,
    ] {
        assert!((curve.ease(0.0) - 0.0).abs() < 1e-9, "{curve:?}");
        assert!((curve.ease(1.0) - 1.0).abs() < 1e-9, "{curve:?}");
        assert!((curve.ease_out(0.0) - 1.0).abs() < 1e-9, "{curve:?}");
        assert!((curve.ease_out(1.0) - 0.0).abs() < 1e-9, "{curve:?}");
        assert_eq!(curve.ease(-2.0), 0.0, "{curve:?}");
        assert_eq!(curve.ease(2.0), 1.0, "{curve:?}");
    }
}

#[test]
fn s_curve_is_symmetric_around_the_midpoint() {
    let curve = CrossfadeCurve::SCurve;
    assert!((curve.ease(0.5) - 0.5).abs() < 1e-9);
    assert!((curve.ease(0.25) + curve.ease(0.75) - 1.0).abs() < 1e-9);
}

#[test]
fn sinusoidal_holds_equal_power_across_the_fade() {
    for step in 0..=100 {
        let progress = f64::from(step) / 100.0;
        let power = CrossfadeCurve::Sinusoidal.ease(progress).powi(2)
            + CrossfadeCurve::Sinusoidal.ease_out(progress).powi(2);
        assert!((power - 1.0).abs() < 1e-6, "progress={progress}");
    }
}

#[test]
fn gapless_needs_a_zero_overlap_and_the_flag() {
    let options = |duration_ms, gapless| CrossfadeOptions {
        duration_ms,
        manual_duration_ms: 3_500,
        curve: CrossfadeCurve::Linear,
        gapless,
    };
    assert!(options(0, true).is_gapless());
    assert!(!options(0, false).is_gapless());
    assert!(!options(1, true).is_gapless());
}
