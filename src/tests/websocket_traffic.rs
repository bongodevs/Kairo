//! Traffic on an open socket: the frame exchange, control-frame answers and slow-client isolation.

use player::source::SourceReference;

use super::*;
use crate::players::GuildId;
use crate::test_support::memory_sources;
use crate::ws::connection::MAX_INBOUND_FRAME;

#[test]
fn stats_cadence_is_one_minute() {
    assert_eq!(STATS_INTERVAL, Duration::from_secs(60));
}

async fn next_op<T>(stream: &mut WebSocketStream<T>, op: &str) -> serde_json::Value
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = next_json(stream).await;
        if message["op"] == op {
            return message;
        }
    }
}

#[tokio::test]
async fn connect_sends_ready_and_registers_the_session() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut stream = connect_async(request(addr, Some("letmein"), Some("170939974227541168")))
        .await
        .expect("handshake")
        .0;

    let frame = stream.next().await.expect("a frame").expect("open");
    let parsed: serde_json::Value =
        serde_json::from_str(frame.into_text().unwrap().as_str()).unwrap();
    assert_eq!(parsed["op"], "ready");
    assert_eq!(parsed["resumed"], false);
    assert_eq!(parsed["sessionId"].as_str().unwrap().len(), SESSION_ID_LEN);
    assert_eq!(store.live(), 1);

    // Stats follow `ready` immediately, then on the minute timer.
    let frame = stream.next().await.expect("a stats frame").expect("open");
    let stats: serde_json::Value =
        serde_json::from_str(frame.into_text().unwrap().as_str()).unwrap();
    assert_eq!(stats["op"], "stats");
    assert_eq!(stats["players"], 0, "a playerless node reports zero");
    assert_eq!(stats["playingPlayers"], 0);
    assert!(
        stats.get("frameStats").is_some_and(|f| f.is_null()),
        "the socket frame keeps frameStats as null"
    );

    // Inbound text is refused, not processed: the connection stays open.
    stream.send(WsMessage::text("hello")).await.unwrap();
    assert_eq!(store.live(), 1);

    drop(stream);
    // Releasing happens in the connection task, so give it a moment.
    for _ in 0..100 {
        if store.live() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.live(), 0);
}

#[tokio::test]
async fn an_inbound_ping_is_answered_with_an_identical_pong() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let mut stream = connect(addr).await;
    assert_eq!(next_json(&mut stream).await["op"], "ready");
    assert_eq!(next_json(&mut stream).await["op"], "stats");

    stream.send(WsMessage::Ping(vec![7, 8, 9].into())).await.expect("ping sent");
    let frame = timeout(FRAME_WAIT, stream.next())
        .await
        .expect("pong in time")
        .expect("open")
        .expect("no transport error");
    match frame {
        WsMessage::Pong(payload) => assert_eq!(&payload[..], &[7, 8, 9][..]),
        other => panic!("expected a pong, got {other:?}"),
    }
}

#[tokio::test]
async fn an_inbound_binary_message_is_ignored_silently() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut stream = connect(addr).await;
    assert_eq!(next_json(&mut stream).await["op"], "ready");
    assert_eq!(next_json(&mut stream).await["op"], "stats");

    // The reference's binary callback is empty; the socket stays alive without a warning.
    stream.send(WsMessage::Binary(vec![1, 2, 3].into())).await.expect("binary sent");
    stream.send(WsMessage::Ping(vec![4].into())).await.expect("ping sent");
    let frame = timeout(FRAME_WAIT, stream.next())
        .await
        .expect("pong in time")
        .expect("open")
        .expect("no transport error");
    match frame {
        WsMessage::Pong(payload) => assert_eq!(&payload[..], &[4][..]),
        other => panic!("expected a pong, got {other:?}"),
    }
    assert_eq!(store.live(), 1);
}

#[tokio::test]
async fn an_unsolicited_pong_is_ignored() {
    let (addr, _store) = spawn(Some("letmein")).await;
    let mut stream = connect(addr).await;
    assert_eq!(next_json(&mut stream).await["op"], "ready");
    assert_eq!(next_json(&mut stream).await["op"], "stats");

    // A pong the client was never asked for earns no reply; the ping after it still earns one, so
    // the socket is alive and the frames after the ignored one arrive normally.
    stream.send(WsMessage::Pong(vec![0].into())).await.expect("pong sent");
    stream.send(WsMessage::Ping(vec![1].into())).await.expect("ping sent");
    let frame = timeout(FRAME_WAIT, stream.next())
        .await
        .expect("pong in time")
        .expect("open")
        .expect("no transport error");
    match frame {
        WsMessage::Pong(payload) => assert_eq!(&payload[..], &[1][..], "only the ping is answered"),
        other => panic!("expected a pong, got {other:?}"),
    }
}

#[tokio::test]
async fn a_stalled_reader_cannot_hold_up_another_session() {
    let (addr, _store) = spawn(Some("letmein")).await;
    // Session A reads nothing at all: its `ready` and `stats` sit in its own task's bounded
    // outbound buffer, and its task awaits only its own socket.
    let _stalled = connect(addr).await;

    // Session B, established while A is stalled, completes its whole exchange immediately
    // (`next_json` is already time-bounded, so a stall here fails rather than hangs).
    let mut healthy = connect(addr).await;
    assert_eq!(next_json(&mut healthy).await["op"], "ready");
    assert_eq!(next_json(&mut healthy).await["op"], "stats");
}

#[tokio::test]
async fn an_active_track_gets_immediate_and_periodic_player_updates() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut stream = connect(addr).await;
    let ready = next_json(&mut stream).await;
    let session_id = ready["sessionId"].as_str().expect("session id");
    assert_eq!(next_json(&mut stream).await["op"], "stats");

    let guild_id = GuildId::new(42);
    let player = store
        .create_player(
            session_id,
            guild_id,
            memory_sources(),
            Arc::new(player::track::codec::TrackCodecs::new()),
            player::playback::PlayerConfig::default(),
        )
        .expect("create player")
        .expect("session");
    player.player().set_paused(true).await.expect("pause player");
    player
        .player()
        .play(player::track::Track::new(SourceReference::new("memory.wav")), false)
        .await
        .expect("play track");

    let immediate = next_op(&mut stream, "playerUpdate").await;
    assert_eq!(immediate["guildId"], "42");
    assert_eq!(immediate["state"]["position"], 0);
    let periodic = next_op(&mut stream, "playerUpdate").await;
    assert_eq!(periodic["guildId"], "42");
    assert!(
        periodic["state"]["time"].as_u64().expect("periodic time")
            >= immediate["state"]["time"].as_u64().expect("immediate time")
    );

    let player = store.remove_player(session_id, guild_id).flatten().expect("remove player");
    player.shutdown().await.expect("shutdown player");
}

/// The close frame that ends a drain, skipping whatever data frames were already queued — the
/// immediate `stats` push and the close race, and only the close is the contract.
async fn next_close<T>(
    stream: &mut WebSocketStream<T>,
) -> tokio_tungstenite::tungstenite::protocol::CloseFrame
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let frame = timeout(FRAME_WAIT, stream.next())
            .await
            .expect("a frame in time")
            .expect("open")
            .expect("no transport error");
        if let WsMessage::Close(Some(frame)) = frame {
            return frame;
        }
    }
}

#[tokio::test]
async fn the_stop_notice_closes_open_sessions_with_1001() {
    let (addr, store, shutdown) = spawn_notified(Some("letmein")).await;
    let mut stream = connect(addr).await;
    assert_eq!(next_json(&mut stream).await["op"], "ready");
    assert_eq!(store.live(), 1);

    // What the graceful-shutdown path does when a signal lands.
    shutdown.send(true).expect("one receiver is live");

    let frame = next_close(&mut stream).await;
    assert_eq!(u16::from(frame.code), 1001, "going away, as the reference hook sends");
    assert!(frame.reason.is_empty(), "and with no reason, likewise");
    // The task ended by itself: the session left the registry without a client disconnect.
    for _ in 0..100 {
        if store.live() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.live(), 0);
}

#[tokio::test]
async fn an_undeliverable_lifecycle_event_closes_the_socket_with_1011() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut stream = connect(addr).await;
    let ready = next_json(&mut stream).await;
    let id = ready["sessionId"].as_str().unwrap().to_owned();

    // What a full lifecycle queue does after its short wait.
    store.cancel_session(&id, "lifecycle backlog");

    let frame = next_close(&mut stream).await;
    assert_eq!(u16::from(frame.code), 1011, "internal error, not a silent drop");
    assert_eq!(frame.reason.as_str(), "lifecycle backlog");
    for _ in 0..100 {
        if store.live() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.live(), 0);
}

#[tokio::test]
async fn a_resumable_session_parks_on_the_notice_like_any_close() {
    let (addr, store, shutdown) = spawn_notified(Some("letmein")).await;
    let mut stream = connect(addr).await;
    let ready = next_json(&mut stream).await;
    let id = ready["sessionId"].as_str().unwrap().to_owned();
    // The PATCH semantics: the live session opts in to resume before the stop lands.
    store.update(&id, Some(true), Some(60));
    assert_eq!(store.live(), 1);

    shutdown.send(true).expect("one receiver is live");
    let frame = next_close(&mut stream).await;
    assert_eq!(u16::from(frame.code), 1001);
    // The connection task ran its own teardown, so the resumable session is parked, not left open.
    for _ in 0..100 {
        if store.live() == 0 && store.paused() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.live(), 0);
    assert_eq!(store.paused(), 1, "draining is a normal close for resume purposes");
}

#[tokio::test]
async fn a_normal_client_close_is_accepted_and_releases_the_session() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut stream = connect(addr).await;
    assert_eq!(next_json(&mut stream).await["op"], "ready");
    stream
        .send(WsMessage::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: "done".into(),
        })))
        .await
        .expect("close sent");

    for _ in 0..100 {
        if store.live() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.live(), 0);
}

#[tokio::test]
async fn an_oversized_inbound_frame_is_rejected() {
    let (addr, store) = spawn(Some("letmein")).await;
    let mut stream = connect(addr).await;
    assert_eq!(next_json(&mut stream).await["op"], "ready");

    stream
        .send(WsMessage::Binary(vec![0u8; MAX_INBOUND_FRAME + 1].into()))
        .await
        .expect("oversized frame sent");
    // The container aborts the connection; no payload is delivered to the application.
    loop {
        let frame = timeout(FRAME_WAIT, stream.next()).await.expect("a result in time");
        match frame {
            Some(Ok(WsMessage::Text(_) | WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
            None | Some(Err(_)) | Some(Ok(WsMessage::Close(_))) => break,
            other => panic!("unexpected frame after an oversized message: {other:?}"),
        }
    }
    for _ in 0..100 {
        if store.live() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.live(), 0);
}
