use super::*;
use crate::players::GuildId;
use tokio::sync::{mpsc, watch};

fn outbound(
    capacity: usize,
) -> (SessionSink, mpsc::Receiver<String>, watch::Receiver<Option<String>>) {
    let (messages, receiver) = mpsc::channel(capacity);
    let (cancel, cancelled) = watch::channel(None);
    (
        SessionSink { messages, updates: Arc::new(UpdateQueue::default()), cancel },
        receiver,
        cancelled,
    )
}

#[test]
fn reserve_claims_an_id_once_until_closed() {
    let store = Arc::new(SessionStore::default());
    assert!(store.reserve("abcdef0123456789"));
    assert!(!store.reserve("abcdef0123456789"));
    store.close("abcdef0123456789");
    assert!(store.reserve("abcdef0123456789"));
}

#[test]
fn node_totals_sum_players_across_open_sessions() {
    let store = Arc::new(SessionStore::default());
    assert_eq!(store.node_totals(), (0, 0));
    store.reserve("s1");
    store.reserve("s2");
    store.set_counts("s1", 3, 1);
    store.set_counts("s2", 2, 2);
    assert_eq!(store.node_totals(), (5, 3));
    store.close("s1");
    assert_eq!(store.node_totals(), (2, 2));
}

#[tokio::test]
async fn node_totals_exclude_a_paused_session_until_it_resumes() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    store.set_counts("s1", 2, 1);
    store.update("s1", Some(true), None);
    assert_eq!(store.node_totals(), (2, 1));
    store.close("s1");
    assert_eq!(store.live(), 0);
    assert_eq!(store.paused(), 1);
    assert_eq!(store.node_totals(), (0, 0));
    assert!(store.begin_resume("s1"));
    assert_eq!(store.node_totals(), (2, 1));
}

#[tokio::test]
async fn closing_a_non_resumable_session_releases_it() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    let player = store
        .create_player(
            "s1",
            crate::players::GuildId::new(42),
            player::source::Sources::new(),
            Arc::new(player::track::codec::TrackCodecs::new()),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    let closed = store.close("s1");
    assert_eq!(store.live(), 0);
    assert_eq!(store.paused(), 0);
    assert!(!store.begin_resume("s1"));
    closed.shutdown().await;
    assert!(player.player().state().await.is_err(), "player owner is still running");
}

#[tokio::test]
async fn a_paused_session_lapses_after_its_timeout() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    store.update("s1", Some(true), Some(0));
    store.close("s1");
    for _ in 0..100 {
        if store.paused() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(store.paused(), 0);
    assert!(!store.begin_resume("s1"));
}

#[tokio::test]
async fn events_from_a_resume_gap_are_not_replayed() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    let (before, mut old_events, _) = outbound(4);
    assert!(store.set_outbound("s1", before));
    store.update("s1", Some(true), Some(60));
    store.close("s1");

    store.send_lifecycle("s1", String::from("during-gap")).await;
    assert!(store.begin_resume("s1"));
    let (after, mut resumed_events, _) = outbound(4);
    assert!(store.set_outbound("s1", after));
    store.send_lifecycle("s1", String::from("after-resume")).await;

    assert_eq!(resumed_events.recv().await.as_deref(), Some("after-resume"));
    assert!(resumed_events.try_recv().is_err());
    assert_eq!(old_events.recv().await, None);
}

#[tokio::test]
async fn a_full_lifecycle_queue_asks_the_socket_to_close_instead_of_dropping() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    let (outbound, mut events, mut cancelled) = outbound(1);
    assert!(store.set_outbound("s1", outbound));

    store.send_lifecycle("s1", String::from("oldest")).await;
    store.send_lifecycle("s1", String::from("newest")).await;

    assert_eq!(events.recv().await.as_deref(), Some("oldest"));
    assert!(events.try_recv().is_err());
    cancelled.changed().await.expect("close signal");
    assert_eq!(cancelled.borrow().as_deref(), Some("lifecycle backlog"));
}

#[tokio::test]
async fn a_full_queue_keeps_only_the_newest_update_per_guild() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    let (outbound, _events, _cancelled) = outbound(1);
    assert!(store.set_outbound("s1", outbound));
    store.send_lifecycle("s1", String::from("holds the queue")).await;

    store.send_update("s1", GuildId::new(1), String::from("old"));
    store.send_update("s1", GuildId::new(1), String::from("new"));
    store.send_update("s1", GuildId::new(2), String::from("other"));

    let sink = store.outbound("s1").expect("sink");
    let mut pending = sink.updates.drain();
    pending.sort();
    assert_eq!(pending, vec![String::from("new"), String::from("other")]);
    assert!(sink.updates.drain().is_empty());
}

#[tokio::test]
async fn timeout_expiry_stops_and_releases_owned_players() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    let player = store
        .create_player(
            "s1",
            crate::players::GuildId::new(42),
            player::source::Sources::new(),
            Arc::new(player::track::codec::TrackCodecs::new()),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    store.update("s1", Some(true), Some(0));
    store.close("s1");

    for _ in 0..100 {
        if store.paused() == 0 && player.player().state().await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(store.paused(), 0);
    assert!(player.player().state().await.is_err(), "player owner is still running");
}

#[tokio::test]
async fn resumable_close_retains_the_player_and_track() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    let player = store
        .create_player(
            "s1",
            crate::players::GuildId::new(42),
            crate::test_support::memory_sources(),
            Arc::new(player::track::codec::TrackCodecs::new()),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    player.player().set_paused(true).await.expect("pause");
    player
        .player()
        .play(player::track::Track::new(player::source::SourceReference::new("memory.wav")), false)
        .await
        .expect("play");
    store.update("s1", Some(true), Some(60));
    store.close("s1");

    assert!(store.players("s1").is_none(), "a parked session is not REST-visible");
    assert!(store.begin_resume("s1"));
    let retained = store
        .players("s1")
        .and_then(|players| players.get(crate::players::GuildId::new(42)))
        .expect("retained player");
    assert!(retained.player().state().await.expect("player state").track.is_some());

    store.update("s1", Some(false), None);
    store.close("s1").shutdown().await;
}

#[test]
fn default_resume_window_is_sixty_seconds() {
    let store = SessionStore::default();
    store.reserve("s1");
    assert_eq!(store.update("s1", None, None), Some((false, 60)));
}

#[tokio::test]
async fn the_store_is_not_idle_until_closed_players_have_stopped() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    store
        .create_player(
            "s1",
            crate::players::GuildId::new(42),
            crate::test_support::memory_sources(),
            Arc::new(player::track::codec::TrackCodecs::new()),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    let closed = store.close("s1");
    assert_eq!(store.live(), 0);

    assert!(
        tokio::time::timeout(Duration::from_millis(20), store.wait_until_idle()).await.is_err(),
        "a closed session with live players is still draining"
    );

    closed.shutdown().await;
    tokio::time::timeout(Duration::from_secs(1), store.wait_until_idle())
        .await
        .expect("idle once the players have stopped");
}

#[tokio::test]
async fn dropping_the_cleanup_handle_still_drains_the_players() {
    let store = Arc::new(SessionStore::default());
    store.reserve("s1");
    let player = store
        .create_player(
            "s1",
            crate::players::GuildId::new(42),
            crate::test_support::memory_sources(),
            Arc::new(player::track::codec::TrackCodecs::new()),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    drop(store.close("s1"));

    tokio::time::timeout(Duration::from_secs(1), store.wait_until_idle())
        .await
        .expect("cancelled cleanup is still drained");
    assert!(player.player().state().await.is_err(), "player owner is still running");
}
