use super::test_support::{OpusPlayer, next_frame};
use crate::filters::{Filters, PassConfig};
#[tokio::test]
async fn cutoff_pass_filters_switch_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("pass", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters {
            high_pass: Some(PassConfig::default()),
            cutoff_low_pass: Some(PassConfig::default()),
            ..Filters::default()
        })
        .await
        .expect("identity filters");
    let (identity, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(identity.position.get(), 20);
    assert_eq!(packet, fixture.packets[1]);

    let high_pass = PassConfig { cutoff_hz: 80, boost: 1.0 };
    let cutoff_low_pass = PassConfig { cutoff_hz: 8_000, boost: 1.0 };
    fixture
        .player
        .set_filters(Filters {
            high_pass: Some(high_pass),
            cutoff_low_pass: Some(cutoff_low_pass),
            ..Filters::default()
        })
        .await
        .expect("enable filters");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 40);
    assert_ne!(packet, fixture.packets[2]);
    let state = fixture.player.state().await.expect("state");
    assert_eq!(state.filters.high_pass, Some(high_pass));
    assert_eq!(state.filters.cutoff_low_pass, Some(cutoff_low_pass));

    fixture.player.set_filters(Filters::default()).await.expect("disable filters");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 60);
    assert_eq!(packet, fixture.packets[3]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
