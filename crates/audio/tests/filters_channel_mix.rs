use super::*;

#[test]
fn channel_mix_identity_gate_matches_the_protocol() {
    assert!(!ChannelMixConfig::default().is_enabled());
}

#[test]
fn channel_mix_uses_the_original_pair_and_correct_matrix_orientation() {
    let swap = ChannelMix::new(ChannelMixConfig {
        left_to_left: 0.0,
        left_to_right: 1.0,
        right_to_left: 1.0,
        right_to_right: 0.0,
    });
    let mut samples = [0.25, -0.75];
    swap.process(&mut samples);
    assert_eq!(samples, [-0.75, 0.25]);

    let mono = ChannelMix::new(ChannelMixConfig {
        left_to_left: 0.5,
        left_to_right: 0.5,
        right_to_left: 0.5,
        right_to_right: 0.5,
    });
    let mut samples = [0.8, -0.2];
    mono.process(&mut samples);
    assert!((samples[0] - 0.3).abs() < 0.000_001);
    assert_eq!(samples[0], samples[1]);
}

#[test]
fn channel_mix_clamps_each_output() {
    let mix = ChannelMix::new(ChannelMixConfig {
        left_to_left: 2.0,
        left_to_right: -2.0,
        right_to_left: 2.0,
        right_to_right: -2.0,
    });
    let mut samples = [1.0, 1.0];
    mix.process(&mut samples);
    assert_eq!(samples, [1.0, -1.0]);
}
