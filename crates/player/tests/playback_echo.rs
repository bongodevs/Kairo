use super::test_support::{OpusPlayer, next_frame};
use crate::filters::{EchoConfig, Filters};
#[tokio::test]
async fn echo_switches_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("echo", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters { echo: Some(EchoConfig::default()), ..Filters::default() })
        .await
        .expect("identity echo");
    let (identity, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(identity.position.get(), 20);
    assert_eq!(packet, fixture.packets[1]);

    let echo = EchoConfig { length_seconds: 0.005, decay: 0.5 };
    fixture
        .player
        .set_filters(Filters { echo: Some(echo), ..Filters::default() })
        .await
        .expect("enable echo");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 40);
    assert_ne!(packet, fixture.packets[2]);
    assert_eq!(fixture.player.state().await.expect("state").filters.echo, Some(echo));

    fixture.player.set_filters(Filters::default()).await.expect("disable echo");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 60);
    assert_eq!(packet, fixture.packets[3]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
