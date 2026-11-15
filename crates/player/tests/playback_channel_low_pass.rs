use super::test_support::{OpusPlayer, next_frame};
use crate::filters::{ChannelMixConfig, Filters, LowPassConfig};
#[tokio::test]
async fn channel_mix_and_low_pass_switch_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("channel-low-pass", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters {
            channel_mix: Some(ChannelMixConfig::default()),
            low_pass: Some(LowPassConfig { smoothing: 1.0 }),
            ..Filters::default()
        })
        .await
        .expect("identity filters");
    let (identity, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(identity.position.get(), 20);
    assert_eq!(packet, fixture.packets[1]);

    let channel_mix = ChannelMixConfig {
        left_to_left: 0.0,
        left_to_right: 1.0,
        right_to_left: 1.0,
        right_to_right: 0.0,
    };
    let low_pass = LowPassConfig { smoothing: 2.0 };
    fixture
        .player
        .set_filters(Filters {
            channel_mix: Some(channel_mix),
            low_pass: Some(low_pass),
            ..Filters::default()
        })
        .await
        .expect("enable filters");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 40);
    assert_ne!(packet, fixture.packets[2]);
    let state = fixture.player.state().await.expect("state");
    assert_eq!(state.filters.channel_mix, Some(channel_mix));
    assert_eq!(state.filters.low_pass, Some(low_pass));

    fixture.player.set_filters(Filters::default()).await.expect("disable filters");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 60);
    assert_eq!(packet, fixture.packets[3]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
