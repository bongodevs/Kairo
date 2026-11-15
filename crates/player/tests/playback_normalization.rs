use super::test_support::{OpusPlayer, next_frame};
use crate::filters::{Filters, NormalizationConfig};
#[tokio::test]
async fn normalization_switches_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("normalization", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters {
            normalization: Some(NormalizationConfig::default()),
            ..Filters::default()
        })
        .await
        .expect("identity normalization");
    let (identity, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(identity.position.get(), 20);
    assert_eq!(packet, fixture.packets[1]);

    let normalization = NormalizationConfig { max_amplitude: 0.2, adaptive: true };
    fixture
        .player
        .set_filters(Filters { normalization: Some(normalization), ..Filters::default() })
        .await
        .expect("enable normalization");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 40);
    assert_ne!(packet, fixture.packets[2]);
    assert_eq!(
        fixture.player.state().await.expect("state").filters.normalization,
        Some(normalization)
    );

    fixture.player.set_filters(Filters::default()).await.expect("disable normalization");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 60);
    assert_eq!(packet, fixture.packets[3]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
