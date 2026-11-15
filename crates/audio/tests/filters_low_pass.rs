use super::*;

#[test]
fn low_pass_identity_gate_matches_the_protocol() {
    assert!(!LowPassConfig { smoothing: 1.0 }.is_enabled());
    assert!(!LowPassConfig { smoothing: -5.0 }.is_enabled());
    assert!(LowPassConfig::default().is_enabled());
}

#[test]
fn low_pass_uses_independent_histories_and_reset_drops_old_audio() {
    let mut low_pass = LowPass::new(LowPassConfig { smoothing: 2.0 });
    let mut samples = [1.0, -1.0, 0.0, 1.0];
    low_pass.process(&mut samples);
    assert_eq!(samples, [1.0, -1.0, 0.5, 0.0]);

    low_pass.reset();
    let mut after_seek = [-0.25, 0.75];
    low_pass.process(&mut after_seek);
    assert_eq!(after_seek, [-0.25, 0.75]);
    assert!(std::mem::size_of::<LowPass>() < 32);
}
