use super::{TapeCurve, TapeEffect, TapeOptions};

fn options(duration_ms: u64, curve: TapeCurve) -> TapeOptions {
    TapeOptions { duration_ms, curve }
}

fn chunk(sample: f32) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(960 * 2);
    for _ in 0..960 {
        pcm.push(sample);
        pcm.push(sample);
    }
    pcm
}

fn feed(tape: &mut TapeEffect, chunks: usize) {
    for _ in 0..chunks {
        tape.push_input(&chunk(0.5), 20);
    }
}

#[test]
fn exponential_curve_has_the_expected_shape() {
    assert_eq!(TapeCurve::Exponential.ease(0.0), 0.0);
    assert!((TapeCurve::Exponential.ease(0.5) - 2f64.powf(-5.0)).abs() < 1e-12);
    assert!((TapeCurve::Exponential.ease(1.0) - 1.0).abs() < 1e-12);
}

#[test]
fn pause_and_resume_move_the_phase_toward_the_target() {
    let mut tape = TapeEffect::new(options(100, TapeCurve::Linear));
    feed(&mut tape, 8);
    assert!(!tape.is_transitioning());

    tape.begin_pause();
    let mut out = Vec::new();
    for _ in 0..4 {
        assert!(tape.try_emit_into(&mut out).is_some());
        assert_eq!(out.len(), 960 * 2);
    }
    assert!(tape.is_transitioning());
    assert!(!tape.is_silent());

    assert!(tape.try_emit_into(&mut out).is_some());
    assert!(tape.is_silent());
    assert!(tape.try_emit_into(&mut out).is_none());
}

#[test]
fn a_reversal_continues_from_the_current_rate() {
    let mut tape = TapeEffect::new(options(200, TapeCurve::Linear));
    feed(&mut tape, 12);
    tape.begin_pause();
    let mut out = Vec::new();
    for _ in 0..5 {
        assert!(tape.try_emit_into(&mut out).is_some());
    }
    tape.begin_resume();
    feed(&mut tape, 20);
    for _ in 0..30 {
        if tape.is_inactive() {
            break;
        }
        assert!(tape.try_emit_into(&mut out).is_some());
    }
    assert!(tape.is_inactive(), "resume should reach full speed");
}

#[test]
fn reset_keeps_the_target_but_drops_buffered_audio() {
    let mut tape = TapeEffect::new(options(100, TapeCurve::Linear));
    tape.push_input(&chunk(0.5), 20);
    tape.begin_pause();
    let mut out = Vec::new();
    assert!(tape.try_emit_into(&mut out).is_some());
    tape.reset();
    assert!(tape.is_silent());
    assert!(tape.try_emit_into(&mut out).is_none());

    tape.begin_resume();
    assert!(tape.try_emit_into(&mut out).is_none(), "input was dropped");
}

#[test]
fn snap_paused_starts_at_a_full_stop() {
    let mut tape = TapeEffect::new(options(500, TapeCurve::SCurve));
    tape.snap_paused();
    assert!(tape.is_silent());
    tape.begin_resume();
    assert!(tape.is_transitioning());
}

#[test]
fn zero_duration_ramps_stop_in_one_frame() {
    let mut tape = TapeEffect::new(options(0, TapeCurve::Linear));
    tape.push_input(&chunk(0.5), 20);
    tape.begin_pause();
    let mut out = Vec::new();
    assert!(tape.try_emit_into(&mut out).is_some());
    assert!(tape.is_silent());
}

#[test]
fn changing_options_mid_ramp_keeps_the_phase_and_buffered_audio() {
    let mut tape = TapeEffect::new(options(100, TapeCurve::Linear));
    feed(&mut tape, 8);
    tape.begin_pause();
    let mut out = Vec::new();
    assert!(tape.try_emit_into(&mut out).is_some());
    tape.set_options(options(500, TapeCurve::Quad));
    assert!(tape.is_transitioning());
    assert!(tape.try_emit_into(&mut out).is_some());
}

#[test]
fn the_ramp_preserves_the_sample_value_at_full_speed() {
    let mut tape = TapeEffect::new(options(100, TapeCurve::Linear));
    tape.push_input(&chunk(0.25), 20);
    let mut out = Vec::new();
    assert_eq!(tape.try_emit_into(&mut out), Some(20));
    assert!(out.iter().all(|sample| (*sample - 0.25).abs() < 1e-9));
}
