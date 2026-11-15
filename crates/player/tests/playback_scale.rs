//! Whole-pipeline release measurements: per-player resident cost, many players decoding and
//! buffering at once, and one player kept running across many tracks. All are `#[ignore]`d, so
//! the gate never pays for them; they answer questions the per-frame benches cannot.

use std::time::Duration;

use super::test_support::OpusPlayer;
use super::*;
use crate::audio::MAX_PACKET_BYTES;
use crate::buffer::BufferRead;
use crate::source::Sources;

const FRAME_MS: u64 = 20;

fn usage() -> (u64, u64) {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let Ok(mut system) = sysinfo::System::new() else {
        return (0, 0);
    };
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
    system.process(pid).map_or((0, 0), |process| (process.accumulated_cpu_time(), process.memory()))
}

fn ticker() -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(Duration::from_millis(FRAME_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measures the resident cost of 256 idle players"]
async fn idle_players_stay_under_one_megabyte_each() {
    const PLAYERS: usize = 256;
    const BUDGET: u64 = 1024 * 1024;

    let (_, before) = usage();
    let mut players = Vec::with_capacity(PLAYERS);
    for _ in 0..PLAYERS {
        let player = spawn_player(Sources::new(), PlayerConfig::default()).expect("spawn player");
        players.push(player);
    }
    tokio::time::sleep(Duration::from_millis(250)).await;

    let (_, after) = usage();
    let growth = after.saturating_sub(before);
    let per_player = growth / PLAYERS as u64;
    eprintln!(
        "{PLAYERS} idle players: {} MiB total, {} KiB each",
        growth / (1024 * 1024),
        per_player / 1024,
    );

    for (_, _, _, guard) in players {
        guard.shutdown().await.expect("shutdown");
    }

    assert!(per_player <= BUDGET, "an idle player used more than 1 MiB: {per_player} bytes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "measures the resident cost of 64 players with full buffers"]
async fn playing_players_stay_under_two_megabytes_each() {
    const PLAYERS: usize = 64;
    const BUDGET: u64 = 2 * 1024 * 1024;

    let (_, before) = usage();
    let mut players = Vec::with_capacity(PLAYERS);
    for index in 0..PLAYERS {
        let opus = OpusPlayer::with_config(
            &format!("memory-playing-{index}"),
            300,
            PlayerConfig::default(),
        )
        .await;
        opus.player.play(opus.track.clone(), false).await.expect("play");
        players.push(opus);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    let (_, after) = usage();
    let growth = after.saturating_sub(before);
    let per_player = growth / PLAYERS as u64;
    eprintln!(
        "{PLAYERS} playing players: {} MiB total, {} KiB each",
        growth / (1024 * 1024),
        per_player / 1024,
    );

    for opus in players {
        opus.shutdown().await;
    }

    assert!(per_player <= BUDGET, "a playing player used more than 2 MiB: {per_player} bytes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "measures 32 concurrent players over five seconds of real time"]
async fn concurrent_playback_has_a_measured_cost() {
    const PLAYERS: usize = 32;
    const WINDOW: Duration = Duration::from_secs(5);
    const WARMUP: Duration = Duration::from_millis(750);

    let mut players = Vec::with_capacity(PLAYERS);
    for index in 0..PLAYERS {
        let opus =
            OpusPlayer::with_config(&format!("concurrent-{index}"), 300, PlayerConfig::default())
                .await;
        opus.player.play(opus.track.clone(), false).await.expect("play");
        players.push(opus);
    }
    tokio::time::sleep(WARMUP).await;

    let (cpu_before, rss_before) = usage();
    let mut consumers = Vec::with_capacity(PLAYERS);
    let mut guards = Vec::with_capacity(PLAYERS);
    let mut paths = Vec::with_capacity(PLAYERS);
    for opus in players {
        let (mut frames, guard, path) = opus.split();
        guards.push(guard);
        paths.push(path);
        consumers.push(tokio::spawn(async move {
            let mut output = [0u8; MAX_PACKET_BYTES];
            let mut ticker = ticker();
            let started = tokio::time::Instant::now();
            let mut frames_read = 0u64;
            let mut empty = 0u64;
            while started.elapsed() < WINDOW {
                ticker.tick().await;
                match frames.read(&mut output) {
                    Ok(BufferRead::Frame(_)) => frames_read += 1,
                    Ok(BufferRead::Empty) => empty += 1,
                    Ok(BufferRead::End | BufferRead::Closed) | Err(_) => break,
                }
            }
            (frames_read, empty)
        }));
    }

    let nominal = WINDOW.as_millis() as u64 / FRAME_MS;
    let mut frames = 0u64;
    let mut empty = 0u64;
    let mut short = Vec::new();
    for (index, consumer) in consumers.into_iter().enumerate() {
        let (read, missed) = consumer.await.expect("consumer");
        if read < nominal * 9 / 10 {
            short.push((index, read));
        }
        frames += read;
        empty += missed;
    }

    let (cpu_after, rss_after) = usage();
    let cores = cpu_after.saturating_sub(cpu_before) as f64 / WINDOW.as_millis() as f64;
    let growth = rss_after.saturating_sub(rss_before);
    eprintln!(
        "{PLAYERS} concurrent players over {} s: {frames} frames, {empty} empty reads, \
         {cores:.2} cores ({:.3} cores/player), {} MiB more resident ({} KiB/player)",
        WINDOW.as_secs(),
        cores / PLAYERS as f64,
        growth / (1024 * 1024),
        growth / 1024 / PLAYERS as u64,
    );

    for guard in guards {
        guard.shutdown().await.expect("shutdown");
    }
    for path in paths {
        tokio::fs::remove_file(path).await.expect("remove fixture");
    }

    assert!(short.is_empty(), "players below 90 % of the nominal rate: {short:?}");
    assert!(
        growth <= (PLAYERS as u64 + 16) * 1024 * 1024,
        "concurrent playback grew RSS by more than 1 MiB per player: {growth} bytes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measures one player across repeated tracks for thirty seconds of real time"]
async fn a_long_playback_session_does_not_grow() {
    const SESSION: Duration = Duration::from_secs(30);
    const SAMPLE_EVERY: Duration = Duration::from_secs(5);
    const TRACK_FRAMES: usize = 250;

    let mut opus = OpusPlayer::new("soak", TRACK_FRAMES).await;
    let player = opus.player.clone();
    let track = opus.track.clone();
    player.play(track.clone(), false).await.expect("play");

    let mut output = [0u8; MAX_PACKET_BYTES];
    let mut ticker = ticker();
    let started = tokio::time::Instant::now();
    let mut next_sample = SAMPLE_EVERY;
    let mut first_rss = 0u64;
    let mut last_rss = 0u64;
    let mut frames = 0u64;
    let mut replays = 0u64;
    while started.elapsed() < SESSION {
        ticker.tick().await;
        match opus.frames.read(&mut output) {
            Ok(BufferRead::Frame(_)) => frames += 1,
            Ok(BufferRead::Empty) => {}
            Ok(BufferRead::End) => {
                player.play(track.clone(), false).await.expect("replay");
                replays += 1;
            }
            Ok(BufferRead::Closed) | Err(_) => break,
        }
        if started.elapsed() >= next_sample {
            let (_, rss) = usage();
            if first_rss == 0 {
                first_rss = rss;
            }
            last_rss = rss;
            eprintln!(
                "soak {} s: {} MiB resident, {frames} frames, {replays} replays",
                next_sample.as_secs(),
                rss / (1024 * 1024),
            );
            next_sample += SAMPLE_EVERY;
        }
    }

    let (_, final_rss) = usage();
    last_rss = final_rss.max(last_rss);
    let growth = last_rss.saturating_sub(first_rss);
    eprintln!(
        "soak end: {frames} frames, {replays} replays, {} MiB after the first sample",
        growth / (1024 * 1024)
    );
    opus.shutdown().await;

    assert!(replays >= 4, "the session only replayed {replays} times");
    assert!(frames >= SESSION.as_millis() as u64 / FRAME_MS * 4 / 5, "the session underran");
    assert!(
        growth <= 8 * 1024 * 1024,
        "a thirty-second session grew RSS by more than 8 MiB: {growth} bytes"
    );
}
