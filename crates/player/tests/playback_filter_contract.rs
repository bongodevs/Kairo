use super::test_support::{OpusPlayer, next_frame};
use super::*;
use crate::filters::{FilterError, Filters};
use crate::opus::OpusError;
use crate::source::Sources;
#[tokio::test]
async fn filter_volume_switches_native_opus_routing() {
    let mut fixture = OpusPlayer::new("filter-volume", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters { volume: 0.5, ..Filters::default() })
        .await
        .expect("enable filter volume");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), first.position.get() + 20);
    assert_ne!(packet, fixture.packets[1]);
    assert_eq!(fixture.player.state().await.expect("state").filters.volume, 0.5);

    fixture.player.set_filters(Filters::default()).await.expect("disable filter volume");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    let index = usize::try_from(restored.position.get() / 20).expect("packet index");
    assert_eq!(restored.position.get(), filtered.position.get() + 20);
    assert_eq!(packet, fixture.packets[index]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn idle_filter_updates_are_validated_before_storage() {
    let (player, _frames, _events, guard) =
        spawn_player(Sources::new(), PlayerConfig::default()).expect("spawn player");
    let error = player
        .set_filters(Filters { volume: f32::NAN, ..Filters::default() })
        .await
        .expect_err("invalid idle filter");
    assert!(matches!(
        error,
        PlayerError::Opus(OpusError::Filter(FilterError::InvalidFilterVolume))
    ));
    assert_eq!(player.state().await.expect("state").filters, Filters::default());

    player
        .set_filters(Filters { volume: 0.5, ..Filters::default() })
        .await
        .expect("valid idle filter");
    assert_eq!(player.state().await.expect("state").filters.volume, 0.5);
    guard.shutdown().await.expect("shutdown");
}
