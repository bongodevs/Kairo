use super::test_support::{OpusPlayer, next_frame};
use crate::filters::{Filters, RotationConfig};
#[tokio::test]
async fn rotation_identity_and_active_state_switch_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("rotation", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters { rotation: Some(RotationConfig::default()), ..Filters::default() })
        .await
        .expect("identity rotation");
    let (identity, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(identity.position.get(), 20);
    assert_eq!(packet, fixture.packets[1]);

    let rotation = RotationConfig { rotation_hz: 0.2 };
    fixture
        .player
        .set_filters(Filters { rotation: Some(rotation), ..Filters::default() })
        .await
        .expect("enable rotation");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 40);
    assert_ne!(packet, fixture.packets[2]);
    assert_eq!(fixture.player.state().await.expect("state").filters.rotation, Some(rotation));

    fixture.player.set_filters(Filters::default()).await.expect("disable rotation");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 60);
    assert_eq!(packet, fixture.packets[3]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
