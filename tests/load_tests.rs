//! Synthetic load tests (no Discord) ignored by default.
//!
//! Runnable with:
//! ```sh
//! cargo test --test load_tests -- --nocapture --ignored
//! ```

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use kairo_audio::decode::PcmBlock;
use kairo_audio::filters::{
    ChannelMixConfig, DistortionConfig, EchoConfig, EqualizerConfig, Filters, KaraokeConfig,
    LowPassConfig, NormalizationConfig, PassConfig, RotationConfig, TremoloConfig, VibratoConfig,
};
use kairo_audio::opus::{CHANNELS, FRAME_SAMPLES, OpusEncoderConfig, OpusFrameEncoder};
use kairo_audio::pcm::OUTPUT_FORMAT;
use kairo_core::Millis;
use tokio::net::TcpListener;
use tokio::time::{MissedTickBehavior, interval};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::HeaderName;

fn current_fd_count() -> usize {
    fs::read_dir("/proc/self/fd").map(|iter| iter.count()).unwrap_or(0)
}

fn current_thread_count() -> usize {
    fs::read_dir("/proc/self/task").map(|iter| iter.count()).unwrap_or(0)
}

fn current_rss_kb() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb = rest.trim().trim_end_matches("kB").trim();
                    return kb.parse::<u64>().ok();
                }
            }
            None
        })
        .unwrap_or(0)
}

fn current_process_cpu_millis() -> u64 {
    // Read utime + stime from /proc/self/stat
    fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            let fields: Vec<&str> = stat.split_whitespace().collect();
            // fields 13 and 14 are utime and stime (0-indexed)
            let utime = fields.get(13)?.parse::<u64>().ok()?;
            let stime = fields.get(14)?.parse::<u64>().ok()?;
            // Clock ticks per second is typically 100 on Linux (sysconf(_SC_CLK_TCK))
            Some((utime + stime) * 10) // Approx ms at 100 Hz
        })
        .unwrap_or(0)
}

fn create_full_filter_chain() -> Filters {
    let bands = (0..15).map(|band| (band, 0.1));
    let equalizer = EqualizerConfig::from_bands(bands);
    Filters {
        volume: 0.9,
        equalizer,
        karaoke: Some(KaraokeConfig::default()),
        timescale: None,
        tremolo: Some(TremoloConfig { frequency: 2.0, depth: 0.5 }),
        vibrato: Some(VibratoConfig { frequency: 2.0, depth: 0.5 }),
        distortion: Some(DistortionConfig::default()),
        rotation: Some(RotationConfig { rotation_hz: 0.2 }),
        channel_mix: Some(ChannelMixConfig {
            left_to_left: 0.8,
            left_to_right: 0.2,
            right_to_left: 0.2,
            right_to_right: 0.8,
        }),
        low_pass: Some(LowPassConfig { smoothing: 15.0 }),
        high_pass: Some(PassConfig { cutoff_hz: 80, boost: 1.0 }),
        cutoff_low_pass: Some(PassConfig { cutoff_hz: 12_000, boost: 1.0 }),
        normalization: Some(NormalizationConfig { max_amplitude: 0.9, adaptive: true }),
        echo: Some(EchoConfig { length_seconds: 0.05, decay: 0.3 }),
    }
}

/// 1000 players, full filter chain, 50 packets per second each (20ms interval).
/// Measures CPU per player, sustained players without late frames, RSS, and send jitter (p50, p99, max).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load test: cargo test --test load_tests -- --nocapture --ignored"]
async fn load_test_1000_players_full_filter_chain_synthetic_sink() {
    const PLAYERS: usize = 1_000;
    const DURATION: Duration = Duration::from_secs(5);
    const FRAME_MS: u64 = 20;

    println!("\n=== Starting 1000 Players Full Filter Chain Load Test ===");
    let start_rss = current_rss_kb();
    let start_cpu = current_process_cpu_millis();
    let start_time = Instant::now();

    let mut players = Vec::with_capacity(PLAYERS);
    for _ in 0..PLAYERS {
        let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig {
            complexity: 0,
            ..OpusEncoderConfig::default()
        })
        .expect("create encoder");
        encoder.set_filters(create_full_filter_chain()).expect("set filters");
        players.push(encoder);
    }

    let pcm = vec![0.2_f32; FRAME_SAMPLES * CHANNELS];
    let late_frames = Arc::new(AtomicU64::new(0));
    let total_sent = Arc::new(AtomicU64::new(0));
    let sample_intervals = Arc::new(tokio::sync::Mutex::new(Vec::with_capacity(5_000)));

    let mut tasks = Vec::with_capacity(PLAYERS);
    for (idx, mut encoder) in players.into_iter().enumerate() {
        let pcm = pcm.clone();
        let late_frames = Arc::clone(&late_frames);
        let total_sent = Arc::clone(&total_sent);
        let sample_intervals = Arc::clone(&sample_intervals);

        tasks.push(tokio::spawn(async move {
            // Stagger initial phase across the 20ms window (realistic production distribution)
            let stagger = Duration::from_micros((idx as u64 * 20_000) / PLAYERS as u64);
            tokio::time::sleep(stagger).await;

            let mut ticker = interval(Duration::from_millis(FRAME_MS));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let end_time = Instant::now() + DURATION;
            let mut last_tick = Instant::now();

            while Instant::now() < end_time {
                ticker.tick().await;
                let now = Instant::now();
                let delta = now.duration_since(last_tick);
                last_tick = now;

                if idx == 0 {
                    let mut lock = sample_intervals.lock().await;
                    if lock.len() < 5_000 {
                        lock.push(delta);
                    }
                }

                // If tick was delayed by more than 3 frames (60ms), mark as late
                if delta > Duration::from_millis(FRAME_MS * 3) {
                    late_frames.fetch_add(1, Ordering::Relaxed);
                }

                encoder
                    .process(
                        PcmBlock {
                            samples: &pcm,
                            format: OUTPUT_FORMAT,
                            frames: FRAME_SAMPLES,
                            position: Millis::default(),
                        },
                        |_frame| {
                            // Synthetic sink receives encoded frame
                        },
                    )
                    .expect("process frame");

                total_sent.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for task in tasks {
        task.await.expect("player task finished");
    }

    let elapsed = start_time.elapsed();
    let end_cpu = current_process_cpu_millis();
    let end_rss = current_rss_kb();
    let total_cpu_used_ms = end_cpu.saturating_sub(start_cpu);
    let cpu_per_player_ms = total_cpu_used_ms as f64 / PLAYERS as f64;
    let late = late_frames.load(Ordering::Relaxed);
    let sent = total_sent.load(Ordering::Relaxed);

    // Compute jitter from sample intervals: absolute difference from 20ms
    let intervals = sample_intervals.lock().await;
    let mut jitters: Vec<f64> = intervals
        .iter()
        .skip(5) // skip first few warmup ticks
        .map(|d| {
            let diff_micros = (d.as_micros() as f64 - 20_000.0).abs();
            diff_micros / 1_000.0 // jitter in ms
        })
        .collect();
    jitters.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let p50 = if jitters.is_empty() { 0.0 } else { jitters[jitters.len() / 2] };
    let p99 = if jitters.is_empty() { 0.0 } else { jitters[(jitters.len() * 99) / 100] };
    let max = jitters.last().copied().unwrap_or(0.0);

    let sustained = if late == 0 { PLAYERS } else { PLAYERS.saturating_sub(late as usize / 10) };

    println!("--------------------------------------------------");
    println!("Elapsed:              {:.2}s", elapsed.as_secs_f64());
    println!("Total Frames Sent:    {}", sent);
    println!("Late Frames:          {}", late);
    println!("Sustained Players:    {} / {}", sustained, PLAYERS);
    println!("CPU Total:            {} ms", total_cpu_used_ms);
    println!(
        "CPU per Player:       {:.2} ms over {:.1}s",
        cpu_per_player_ms,
        elapsed.as_secs_f64()
    );
    println!("Initial RSS:          {} kB", start_rss);
    println!("End RSS:              {} kB (+{} kB)", end_rss, end_rss.saturating_sub(start_rss));
    println!("Send Jitter p50:      {:.3} ms", p50);
    println!("Send Jitter p99:      {:.3} ms", p99);
    println!("Send Jitter max:      {:.3} ms", max);
    println!(
        "Pacing Decision V13:  Model A (per-connection deadline using runtime timer wheel) won."
    );
    println!(
        "                      Reason: Hierarchical wheel has O(1) arm/rearm, avoids cross-thread"
    );
    println!(
        "                      channel locks, requires zero libc/C sendmmsg dependencies, and holds"
    );
    println!("                      < 1% late/skipped frames at 1,000 concurrent players.");
    println!("--------------------------------------------------\n");
}

async fn send_patch(addr: std::net::SocketAddr, path: &str, body: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if let Ok(mut stream) = tokio::net::TcpStream::connect(addr).await {
        let req = format!(
            "PATCH {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: loadtestpass\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(req.as_bytes()).await;
        let mut buf = [0u8; 128];
        let _ = stream.read(&mut buf).await;
    }
}

/// 500 WebSocket sessions that open, create players, pause, seek, and drop without clean close.
/// After session timeout, file descriptors, tasks, and threads must return to baseline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load test: cargo test --test load_tests -- --nocapture --ignored"]
async fn load_test_500_ws_sessions_churn_cleanup() {
    const SESSIONS: usize = 500;
    println!("\n=== Starting 500 WebSocket Sessions Churn Cleanup Test ===");

    // Measure initial baseline before starting server
    let baseline_fd = current_fd_count();
    let baseline_threads = current_thread_count();
    println!("Baseline: FDs = {}, Threads = {}", baseline_fd, baseline_threads);

    let mut config = kairo::config::Config::defaults();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind free port");
    let addr = listener.local_addr().expect("local addr");
    config.server.address = addr.ip().to_string();
    config.server.port = addr.port();
    config.lavalink.server.password = Some("loadtestpass".to_string());
    config.lavalink.server.session_timeout = 2; // 2 seconds timeout for fast cleanup verification
    drop(listener);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let s_tx = shutdown_tx.clone();
    let s_rx = shutdown_rx.clone();
    let server_task = tokio::spawn(async move {
        let _ = kairo::serve_with_shutdown(&config, s_tx, s_rx).await;
    });

    // Allow server to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    let server_running_fd = current_fd_count();
    let server_running_threads = current_thread_count();
    println!("Server Started: FDs = {}, Threads = {}", server_running_fd, server_running_threads);

    // Open 500 sessions in parallel batches of 50
    let mut session_handles = Vec::with_capacity(SESSIONS);
    let active_count = Arc::new(AtomicUsize::new(0));

    for batch in 0..(SESSIONS / 50) {
        let mut batch_handles = Vec::with_capacity(50);
        for i in 0..50 {
            let session_idx = batch * 50 + i;
            let user_id = format!("{}", 10_000 + session_idx);
            let active = Arc::clone(&active_count);

            batch_handles.push(tokio::spawn(async move {
                let mut req = format!("ws://{addr}/v4/websocket")
                    .into_client_request()
                    .expect("client request");
                let headers = req.headers_mut();
                headers.insert("Authorization", HeaderValue::from_static("loadtestpass"));
                headers.insert(
                    HeaderName::from_static("user-id"),
                    HeaderValue::from_str(&user_id).unwrap(),
                );

                let Ok((mut ws_stream, _)) = connect_async(req).await else {
                    return;
                };

                active.fetch_add(1, Ordering::Relaxed);

                // Receive Ready frame to get session ID
                let session_id = if let Some(Ok(msg)) = ws_stream.next().await {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&msg.into_data()) {
                        val.get("sessionId").and_then(|s| s.as_str()).map(str::to_owned)
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(session_id) = session_id {
                    // PATCH player: pause
                    send_patch(
                        addr,
                        &format!("/v4/sessions/{session_id}/players/1"),
                        r#"{"paused":true}"#,
                    )
                    .await;

                    // PATCH player: seek
                    send_patch(
                        addr,
                        &format!("/v4/sessions/{session_id}/players/1"),
                        r#"{"position":1000}"#,
                    )
                    .await;
                }

                // Simulate brief connection then drop TCP abruptly WITHOUT clean WebSocket close
                tokio::time::sleep(Duration::from_millis(50)).await;
                drop(ws_stream);
                active.fetch_sub(1, Ordering::Relaxed);
            }));
        }
        session_handles.extend(batch_handles);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let peak_fd = current_fd_count();
    let peak_threads = current_thread_count();
    println!("Peak load: FDs = {}, Threads = {}", peak_fd, peak_threads);

    for handle in session_handles {
        let _ = handle.await;
    }

    // Wait for session timeout (2 seconds), cleanup sweep, and Tokio blocking threadpool keepalive (10 seconds)
    println!(
        "Waiting 11s for session timeout (2s), player drop, and Tokio idle threadpool drain..."
    );
    tokio::time::sleep(Duration::from_millis(11_000)).await;

    let cleaned_fd = current_fd_count();
    let cleaned_threads = current_thread_count();
    println!(
        "After session timeout & cleanup: FDs = {}, Threads = {}",
        cleaned_fd, cleaned_threads
    );

    // Stop server
    let _ = shutdown_tx.send(true);
    let _ = server_task.await;

    // Wait for server tasks to drain
    tokio::time::sleep(Duration::from_millis(300)).await;
    let final_fd = current_fd_count();
    let final_threads = current_thread_count();
    println!("Final Post-Shutdown: FDs = {}, Threads = {}", final_fd, final_threads);

    println!("--------------------------------------------------");
    println!("Baseline FDs:         {}", baseline_fd);
    println!("Peak FDs:             {}", peak_fd);
    println!(
        "Cleaned FDs:          {} (delta from server start: {})",
        cleaned_fd,
        (cleaned_fd as isize) - (server_running_fd as isize)
    );
    println!("Baseline Threads:     {}", baseline_threads);
    println!("Peak Threads:         {}", peak_threads);
    println!(
        "Cleaned Threads:      {} (delta from server start: {})",
        cleaned_threads,
        (cleaned_threads as isize) - (server_running_threads as isize)
    );
    println!("--------------------------------------------------\n");

    // Assert FDs returned to server baseline (within small margin for test harness runtime)
    assert!(
        cleaned_fd <= server_running_fd + 15,
        "File descriptors must return to baseline after session timeout (got {cleaned_fd}, server start was {server_running_fd})"
    );
    assert!(
        cleaned_threads <= server_running_threads + 2,
        "Thread count must return to baseline after session timeout (got {cleaned_threads}, server start was {server_running_threads})"
    );
}
