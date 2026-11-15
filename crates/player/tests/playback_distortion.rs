use super::test_support::{OpusPlayer, next_frame};
use crate::filters::{DistortionConfig, Filters};
#[tokio::test]
async fn distortion_identity_and_active_state_switch_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("distortion", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters {
            distortion: Some(DistortionConfig::default()),
            ..Filters::default()
        })
        .await
        .expect("identity distortion");
    let (identity, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(identity.position.get(), 20);
    assert_eq!(packet, fixture.packets[1]);

    let distortion = DistortionConfig { sin_offset: 0.2, ..DistortionConfig::default() };
    fixture
        .player
        .set_filters(Filters { distortion: Some(distortion), ..Filters::default() })
        .await
        .expect("enable distortion");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 40);
    assert_ne!(packet, fixture.packets[2]);
    assert_eq!(fixture.player.state().await.expect("state").filters.distortion, Some(distortion));

    fixture.player.set_filters(Filters::default()).await.expect("disable distortion");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 60);
    assert_eq!(packet, fixture.packets[3]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
