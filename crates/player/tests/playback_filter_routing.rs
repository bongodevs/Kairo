use super::test_support::{OpusPlayer, next_frame};
use crate::filters::{
    EqualizerConfig, Filters, KaraokeConfig, TimescaleConfig, TremoloConfig, VibratoConfig,
};
#[tokio::test]
async fn equalizer_leaves_and_reenters_native_opus_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("equalizer", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    let equalizer = EqualizerConfig::from_bands([(0, 0.5)]);
    fixture
        .player
        .set_filters(Filters { equalizer, ..Filters::default() })
        .await
        .expect("enable equalizer");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 20);
    assert_ne!(packet, fixture.packets[1]);
    assert_eq!(fixture.player.state().await.expect("state").filters.equalizer, equalizer);

    fixture.player.set_filters(Filters::default()).await.expect("disable equalizer");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    let index = usize::try_from(restored.position.get() / 20).expect("packet index");
    assert_eq!(restored.position.get(), filtered.position.get() + 20);
    assert_eq!(packet, fixture.packets[index]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn karaoke_leaves_and_reenters_native_opus_passthrough_without_a_time_jump() {
    let mut fixture = OpusPlayer::new("karaoke", 40).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(packet, fixture.packets[0]);

    fixture
        .player
        .set_filters(Filters { karaoke: Some(KaraokeConfig::default()), ..Filters::default() })
        .await
        .expect("enable karaoke");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), first.position.get() + 20);
    assert_ne!(packet, fixture.packets[1]);
    assert_eq!(
        fixture.player.state().await.expect("state").filters.karaoke,
        Some(KaraokeConfig::default())
    );

    fixture.player.set_filters(Filters::default()).await.expect("disable karaoke");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    let index = usize::try_from(restored.position.get() / 20).expect("packet index");
    assert_eq!(restored.position.get(), filtered.position.get() + 20);
    assert_eq!(packet, fixture.packets[index]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn timescale_uses_source_positions_when_leaving_and_reentering_passthrough() {
    let mut fixture = OpusPlayer::new("timescale", 160).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    let timescale = TimescaleConfig { speed: 2.0, ..TimescaleConfig::default() };
    fixture
        .player
        .set_filters(Filters { timescale: Some(timescale), ..Filters::default() })
        .await
        .expect("enable timescale");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 20);
    assert_ne!(packet, fixture.packets[1]);
    let (filtered_next, _) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered_next.position.get(), 60);
    assert_eq!(fixture.player.state().await.expect("state").filters.timescale, Some(timescale));

    fixture.player.set_filters(Filters::default()).await.expect("disable timescale");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 100);
    assert_eq!(packet, fixture.packets[5]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn modulation_filters_reenter_passthrough_and_reject_bad_updates() {
    let mut fixture = OpusPlayer::new("modulation", 160).await;
    fixture.player.play(fixture.track.clone(), false).await.expect("play");
    let (first, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(first.position.get(), 0);
    assert_eq!(packet, fixture.packets[0]);

    let tremolo = TremoloConfig::default();
    fixture
        .player
        .set_filters(Filters { tremolo: Some(tremolo), ..Filters::default() })
        .await
        .expect("enable tremolo");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 20);
    assert_ne!(packet, fixture.packets[1]);
    assert_eq!(fixture.player.state().await.expect("state").filters.tremolo, Some(tremolo));

    fixture.player.set_filters(Filters::default()).await.expect("disable tremolo");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 40);
    assert_eq!(packet, fixture.packets[2]);
    let invalid = VibratoConfig { frequency: 15.0, ..VibratoConfig::default() };
    assert!(
        fixture
            .player
            .set_filters(Filters { vibrato: Some(invalid), ..Filters::default() })
            .await
            .is_err()
    );
    assert_eq!(fixture.player.state().await.expect("state").filters.vibrato, None);
    let (after_error, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(after_error.position.get(), 60);
    assert_eq!(packet, fixture.packets[3]);

    let vibrato = VibratoConfig::default();
    fixture
        .player
        .set_filters(Filters { vibrato: Some(vibrato), ..Filters::default() })
        .await
        .expect("enable vibrato");
    let (filtered, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(filtered.position.get(), 80);
    assert_ne!(packet, fixture.packets[4]);
    fixture.player.set_filters(Filters::default()).await.expect("disable vibrato");
    let (restored, packet) = next_frame(&mut fixture.frames).await;
    assert_eq!(restored.position.get(), 100);
    assert_eq!(packet, fixture.packets[5]);
    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}
