use std::time::Duration;

use super::test_support::{OpusPlayer, next_frame};
use crate::decode::Millis;
use crate::track::marker::{MarkerState, TrackMarker, TrackMarkerTracker};

#[test]
fn track_marker_tracker_direct_lifecycle() {
    let tracker = TrackMarkerTracker::new();

    // 1. Late
    let (marker, mut rx) = TrackMarker::channel(Millis::new(100));
    tracker.add(marker, Millis::new(150));
    assert_eq!(rx.try_recv(), Ok(MarkerState::Late));

    // 2. Reached
    let (marker, mut rx) = TrackMarker::channel(Millis::new(200));
    tracker.add(marker, Millis::new(100));
    tracker.check_playback(Millis::new(199));
    assert!(rx.try_recv().is_err());
    tracker.check_playback(Millis::new(200));
    assert_eq!(rx.try_recv(), Ok(MarkerState::Reached));

    // 3. Bypassed
    let (marker, mut rx) = TrackMarker::channel(Millis::new(300));
    tracker.add(marker, Millis::new(100));
    tracker.check_seek(Millis::new(350));
    assert_eq!(rx.try_recv(), Ok(MarkerState::Bypassed));

    // 4. Removed
    let (marker, mut rx) = TrackMarker::channel(Millis::new(400));
    let id = marker.id();
    tracker.add(marker, Millis::new(100));
    assert!(tracker.remove(id));
    assert_eq!(rx.try_recv(), Ok(MarkerState::Removed));

    // 5. Overwritten
    let (marker1, mut rx1) = TrackMarker::channel(Millis::new(500));
    let (marker2, mut rx2) = TrackMarker::channel(Millis::new(600));
    tracker.set(Some(marker1), Millis::new(100));
    tracker.set(Some(marker2), Millis::new(100));
    assert_eq!(rx1.try_recv(), Ok(MarkerState::Overwritten));
    assert!(rx2.try_recv().is_err());

    // 6. Stopped
    tracker.trigger_all(MarkerState::Stopped);
    assert_eq!(rx2.try_recv(), Ok(MarkerState::Stopped));

    // 7. Ended
    let (marker, mut rx) = TrackMarker::channel(Millis::new(700));
    tracker.add(marker, Millis::new(100));
    tracker.trigger_all(MarkerState::Ended);
    assert_eq!(rx.try_recv(), Ok(MarkerState::Ended));
}

#[tokio::test]
async fn marker_reaches_in_real_playback() {
    let mut fixture = OpusPlayer::new("marker-reached", 40).await;
    let (marker, rx) = TrackMarker::channel(Millis::new(40));

    fixture.player.add_marker(marker).await.expect("add marker");
    fixture.player.play(fixture.track.clone(), false).await.expect("play");

    // Frame 0: pos 0
    let (f0, _) = next_frame(&mut fixture.frames).await;
    assert_eq!(f0.position.get(), 0);

    // Frame 1: pos 20
    let (f1, _) = next_frame(&mut fixture.frames).await;
    assert_eq!(f1.position.get(), 20);

    // Frame 2: pos 40 -> reaches marker
    let (f2, _) = next_frame(&mut fixture.frames).await;
    assert_eq!(f2.position.get(), 40);

    let state = tokio::time::timeout(Duration::from_millis(500), rx)
        .await
        .expect("marker timeout")
        .expect("marker channel");
    assert_eq!(state, MarkerState::Reached);

    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn marker_bypassed_on_seek() {
    let mut fixture = OpusPlayer::new("marker-bypassed", 40).await;
    let (marker, rx) = TrackMarker::channel(Millis::new(60));

    fixture.player.add_marker(marker).await.expect("add marker");
    fixture.player.play(fixture.track.clone(), false).await.expect("play");

    let _ = next_frame(&mut fixture.frames).await;

    // Seek past 60 to 100
    fixture.player.seek(Millis::new(100)).await.expect("seek");

    let state = tokio::time::timeout(Duration::from_millis(500), rx)
        .await
        .expect("marker timeout")
        .expect("marker channel");
    assert_eq!(state, MarkerState::Bypassed);

    fixture.player.stop().await.expect("stop");
    fixture.shutdown().await;
}

#[tokio::test]
async fn marker_stopped_on_player_stop() {
    let mut fixture = OpusPlayer::new("marker-stopped", 40).await;
    let (marker, rx) = TrackMarker::channel(Millis::new(200));

    fixture.player.add_marker(marker).await.expect("add marker");
    fixture.player.play(fixture.track.clone(), false).await.expect("play");

    let _ = next_frame(&mut fixture.frames).await;

    fixture.player.stop().await.expect("stop");

    let state = tokio::time::timeout(Duration::from_millis(500), rx)
        .await
        .expect("marker timeout")
        .expect("marker channel");
    assert_eq!(state, MarkerState::Stopped);

    fixture.shutdown().await;
}

#[tokio::test]
async fn marker_removed_and_overwritten_via_player() {
    let fixture = OpusPlayer::new("marker-remove-overwrite", 40).await;
    let (marker1, rx1) = TrackMarker::channel(Millis::new(200));
    let id1 = marker1.id();
    let (marker2, rx2) = TrackMarker::channel(Millis::new(200));
    let (marker3, rx3) = TrackMarker::channel(Millis::new(200));

    // Add marker1, then remove it
    fixture.player.add_marker(marker1).await.expect("add marker1");
    let removed = fixture.player.remove_marker(id1).await.expect("remove marker1");
    assert!(removed);
    assert_eq!(rx1.await, Ok(MarkerState::Removed));

    // Set marker2, then set marker3 (overwriting marker2)
    fixture.player.set_marker(Some(marker2)).await.expect("set marker2");
    fixture.player.set_marker(Some(marker3)).await.expect("set marker3");
    assert_eq!(rx2.await, Ok(MarkerState::Overwritten));

    // Clear marker3 with None (triggers Removed)
    fixture.player.set_marker(None).await.expect("clear marker3");
    assert_eq!(rx3.await, Ok(MarkerState::Removed));

    fixture.shutdown().await;
}
