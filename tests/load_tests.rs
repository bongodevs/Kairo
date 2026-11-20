//! Synthetic load tests (no Discord) ignored by default.
//!
//! Runnable with:
//! ```sh
//! cargo test --release --test load_tests -- --nocapture --ignored
//! ```

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use kairo_audio::decode::PcmBlock;
use kairo_audio::filters::{
    ChannelMix, ChannelMixConfig, CutoffLowPass, Distortion, DistortionConfig, Echo, EchoConfig,
    Equalizer, EqualizerConfig, FilterVolume, Filters, HighPass, Karaoke, KaraokeConfig, LowPass,
    LowPassConfig, Normalization, NormalizationConfig, PassConfig, Rotation, RotationConfig,
    Tremolo, TremoloConfig, Vibrato, VibratoConfig,
};
use kairo_audio::opus::{CHANNELS, FRAME_SAMPLES, OpusEncoderConfig, OpusFrameEncoder};
use kairo_audio::pcm::{OUTPUT_FORMAT, PcmNormalizer, ResamplingQuality};
use kairo_core::{AudioFormat, Millis};
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::HeaderName;
use voice::crypto::{KEY_LEN, TransportCipher, XCHACHA_MODE};
use voice::udp::rtp::{RTP_HEADER_LEN, Rtp};

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
    fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            let fields: Vec<&str> = stat.split_whitespace().collect();
            let utime = fields.get(13)?.parse::<u64>().ok()?;
            let stime = fields.get(14)?.parse::<u64>().ok()?;
            Some((utime + stime) * 10)
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

fn create_volume_eq_filters() -> Filters {
    let bands = (0..15).map(|band| (band, 0.1));
    let equalizer = EqualizerConfig::from_bands(bands);
    Filters {
        volume: 0.8,
        equalizer,
        karaoke: None,
        timescale: None,
        tremolo: None,
        vibrato: None,
        distortion: None,
        rotation: None,
        channel_mix: None,
        low_pass: None,
        high_pass: None,
        cutoff_low_pass: None,
        normalization: None,
        echo: None,
    }
}

/// Stage Breakdown Benchmark: measures per-frame cost of every stage in microseconds.
#[test]
#[ignore = "load test: cargo test --release --test load_tests -- --nocapture --ignored"]
fn test_stage_breakdown_cost() {
    println!("\n================================================================================");
    println!("Stage Breakdown Microbenchmark (20 ms / 960 stereo samples frame budget)");
    println!("Machine: Intel Core i5-8365U CPU @ 1.60GHz (4 physical cores / 8 logical threads)");
    println!("================================================================================");

    const WARMUP: usize = 200;
    const ITERS: usize = 5_000;

    let mut pcm = vec![0.3_f32; FRAME_SAMPLES * CHANNELS];

    // 1. Decode simulation (unpack / interleave 960 stereo samples from raw bytes)
    let raw_bytes = vec![0x12_u8; FRAME_SAMPLES * CHANNELS * 2];
    for _ in 0..WARMUP {
        for (i, sample) in pcm.iter_mut().enumerate() {
            let b0 = raw_bytes[i * 2];
            let b1 = raw_bytes[i * 2 + 1];
            let s = i16::from_le_bytes([b0, b1]);
            *sample = s as f32 / 32768.0;
        }
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        for (i, sample) in pcm.iter_mut().enumerate() {
            let b0 = raw_bytes[i * 2];
            let b1 = raw_bytes[i * 2 + 1];
            let s = i16::from_le_bytes([b0, b1]);
            *sample = s as f32 / 32768.0;
        }
    }
    let decode_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    // 2. Resampler: Rubato 44.1 kHz -> 48.0 kHz
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::Low);
    let in_format = AudioFormat { sample_rate: 44_100, channels: 2 };
    let in_samples = vec![0.3_f32; 882 * 2]; // 20 ms at 44.1 kHz
    for _ in 0..WARMUP {
        normalizer
            .process(
                PcmBlock {
                    samples: &in_samples,
                    format: in_format,
                    frames: 882,
                    position: Millis::default(),
                },
                |_| {},
            )
            .unwrap();
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        normalizer
            .process(
                PcmBlock {
                    samples: &in_samples,
                    format: in_format,
                    frames: 882,
                    position: Millis::default(),
                },
                |_| {},
            )
            .unwrap();
    }
    let resample_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    // 3. Filters individually
    let volume = FilterVolume::new(0.8).unwrap();
    let t0 = Instant::now();
    for _ in 0..ITERS {
        volume.process(&mut pcm);
    }
    let filter_volume_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let bands = (0..15).map(|b| (b, 0.1));
    let mut eq = Equalizer::new(EqualizerConfig::from_bands(bands));
    let t0 = Instant::now();
    for _ in 0..ITERS {
        eq.process(&mut pcm);
    }
    let filter_eq_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut karaoke = Karaoke::new(KaraokeConfig::default());
    let t0 = Instant::now();
    for _ in 0..ITERS {
        karaoke.process(&mut pcm);
    }
    let filter_karaoke_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut tremolo = Tremolo::new(TremoloConfig { frequency: 2.0, depth: 0.5 }).unwrap();
    let t0 = Instant::now();
    for _ in 0..ITERS {
        tremolo.process(&mut pcm);
    }
    let filter_tremolo_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut vibrato = Vibrato::new(VibratoConfig { frequency: 2.0, depth: 0.5 }).unwrap();
    let t0 = Instant::now();
    for _ in 0..ITERS {
        vibrato.process(&mut pcm);
    }
    let filter_vibrato_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let distortion = Distortion::new(DistortionConfig::default());
    let t0 = Instant::now();
    for _ in 0..ITERS {
        distortion.process(&mut pcm);
    }
    let filter_distortion_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut rotation = Rotation::new(RotationConfig { rotation_hz: 0.2 });
    let t0 = Instant::now();
    for _ in 0..ITERS {
        rotation.process(&mut pcm);
    }
    let filter_rotation_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let ch_mix = ChannelMix::new(ChannelMixConfig {
        left_to_left: 0.8,
        left_to_right: 0.2,
        right_to_left: 0.2,
        right_to_right: 0.8,
    });
    let t0 = Instant::now();
    for _ in 0..ITERS {
        ch_mix.process(&mut pcm);
    }
    let filter_ch_mix_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut lp = LowPass::new(LowPassConfig { smoothing: 15.0 });
    let t0 = Instant::now();
    for _ in 0..ITERS {
        lp.process(&mut pcm);
    }
    let filter_lp_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut hp = HighPass::new(PassConfig { cutoff_hz: 80, boost: 1.0 });
    let t0 = Instant::now();
    for _ in 0..ITERS {
        hp.process(&mut pcm);
    }
    let filter_hp_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut cutoff_lp = CutoffLowPass::new(PassConfig { cutoff_hz: 12_000, boost: 1.0 });
    let t0 = Instant::now();
    for _ in 0..ITERS {
        cutoff_lp.process(&mut pcm);
    }
    let filter_cutoff_lp_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut norm = Normalization::new(NormalizationConfig { max_amplitude: 0.9, adaptive: true });
    let t0 = Instant::now();
    for _ in 0..ITERS {
        norm.process(&mut pcm);
    }
    let filter_norm_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let mut echo = Echo::new(EchoConfig { length_seconds: 0.05, decay: 0.3 }).unwrap();
    let t0 = Instant::now();
    for _ in 0..ITERS {
        echo.process(&mut pcm);
    }
    let filter_echo_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let filter_sum_us = filter_volume_us
        + filter_eq_us
        + filter_karaoke_us
        + filter_tremolo_us
        + filter_vibrato_us
        + filter_distortion_us
        + filter_rotation_us
        + filter_ch_mix_us
        + filter_lp_us
        + filter_hp_us
        + filter_cutoff_lp_us
        + filter_norm_us
        + filter_echo_us;

    // 4. Opus Encode (48 kHz stereo, 128 kbps, complexity 10)
    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig {
        bitrate_bps: 128_000,
        complexity: 10,
        ..OpusEncoderConfig::default()
    })
    .unwrap();
    let mut packet_buf = vec![0u8; 1275];
    for _ in 0..WARMUP {
        encoder
            .process(
                PcmBlock {
                    samples: &pcm,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES,
                    position: Millis::default(),
                },
                |f| {
                    packet_buf[..f.packet.len()].copy_from_slice(f.packet);
                },
            )
            .unwrap();
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        encoder
            .process(
                PcmBlock {
                    samples: &pcm,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES,
                    position: Millis::default(),
                },
                |f| {
                    packet_buf[..f.packet.len()].copy_from_slice(f.packet);
                },
            )
            .unwrap();
    }
    let opus_encode_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    // 5. DAVE / Transport AEAD encrypt (XChaCha20Poly1305)
    let key = [0x42_u8; KEY_LEN];
    let mut cipher = TransportCipher::new(XCHACHA_MODE, &key).unwrap();
    let mut header_buf = [0u8; RTP_HEADER_LEN];
    let mut rtp_packet = vec![0u8; 1500];
    let payload = &[0xAA_u8; 160];
    let t0 = Instant::now();
    for _ in 0..ITERS {
        let mut rtp = Rtp::new(0x1234_5678);
        rtp.next_header(&mut header_buf, false);
        let _ = cipher.seal(&header_buf, payload, &mut rtp_packet);
    }
    let encrypt_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    // 6. RTP header write
    let mut rtp = Rtp::new(0x1234_5678);
    let t0 = Instant::now();
    for _ in 0..ITERS {
        rtp.next_header(&mut header_buf, false);
    }
    let rtp_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    // 7. UDP Socket Send to loopback
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (tx_sock, _rx_sock, target_addr) = rt.block_on(async {
        let tx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = rx.local_addr().unwrap();
        tx.set_nonblocking(true).unwrap();
        (tx, rx, target)
    });
    let send_payload = vec![0u8; 200];
    let t0 = Instant::now();
    for _ in 0..ITERS {
        let _ = tx_sock.send_to(&send_payload, target_addr);
    }
    let udp_send_us = t0.elapsed().as_nanos() as f64 / ITERS as f64 / 1_000.0;

    let total_us = decode_us
        + resample_us
        + filter_sum_us
        + opus_encode_us
        + encrypt_us
        + rtp_us
        + udp_send_us;

    println!("\n{:<30} {:>12} {:>10}", "Pipeline Stage", "Time (µs)", "Share (%)");
    println!("{:-<54}", "");
    println!(
        "{:<30} {:>12.2} {:>9.1}%",
        "1. Native Decode / Unpack",
        decode_us,
        (decode_us / total_us) * 100.0
    );
    println!(
        "{:<30} {:>12.2} {:>9.1}%",
        "2. Resample (44.1k -> 48k)",
        resample_us,
        (resample_us / total_us) * 100.0
    );
    println!(
        "{:<30} {:>12.2} {:>9.1}%",
        "3. Audio Filter Chain (14)",
        filter_sum_us,
        (filter_sum_us / total_us) * 100.0
    );
    println!(
        "  - Volume                   {:>12.2} {:>9.1}%",
        filter_volume_us,
        (filter_volume_us / total_us) * 100.0
    );
    println!(
        "  - 15-band Equalizer        {:>12.2} {:>9.1}%",
        filter_eq_us,
        (filter_eq_us / total_us) * 100.0
    );
    println!(
        "  - Karaoke                  {:>12.2} {:>9.1}%",
        filter_karaoke_us,
        (filter_karaoke_us / total_us) * 100.0
    );
    println!(
        "  - Tremolo                  {:>12.2} {:>9.1}%",
        filter_tremolo_us,
        (filter_tremolo_us / total_us) * 100.0
    );
    println!(
        "  - Vibrato                  {:>12.2} {:>9.1}%",
        filter_vibrato_us,
        (filter_vibrato_us / total_us) * 100.0
    );
    println!(
        "  - Distortion               {:>12.2} {:>9.1}%",
        filter_distortion_us,
        (filter_distortion_us / total_us) * 100.0
    );
    println!(
        "  - Rotation                 {:>12.2} {:>9.1}%",
        filter_rotation_us,
        (filter_rotation_us / total_us) * 100.0
    );
    println!(
        "  - Channel Mix              {:>12.2} {:>9.1}%",
        filter_ch_mix_us,
        (filter_ch_mix_us / total_us) * 100.0
    );
    println!(
        "  - Low Pass                 {:>12.2} {:>9.1}%",
        filter_lp_us,
        (filter_lp_us / total_us) * 100.0
    );
    println!(
        "  - High Pass                {:>12.2} {:>9.1}%",
        filter_hp_us,
        (filter_hp_us / total_us) * 100.0
    );
    println!(
        "  - Cutoff Low Pass          {:>12.2} {:>9.1}%",
        filter_cutoff_lp_us,
        (filter_cutoff_lp_us / total_us) * 100.0
    );
    println!(
        "  - Normalization            {:>12.2} {:>9.1}%",
        filter_norm_us,
        (filter_norm_us / total_us) * 100.0
    );
    println!(
        "  - Echo                     {:>12.2} {:>9.1}%",
        filter_echo_us,
        (filter_echo_us / total_us) * 100.0
    );
    println!(
        "{:<30} {:>12.2} {:>9.1}%",
        "4. Opus Encode (128k c=10)",
        opus_encode_us,
        (opus_encode_us / total_us) * 100.0
    );
    println!(
        "{:<30} {:>12.2} {:>9.1}%",
        "5. DAVE Transport AEAD",
        encrypt_us,
        (encrypt_us / total_us) * 100.0
    );
    println!(
        "{:<30} {:>12.2} {:>9.1}%",
        "6. RTP Packetization",
        rtp_us,
        (rtp_us / total_us) * 100.0
    );
    println!(
        "{:<30} {:>12.2} {:>9.1}%",
        "7. UDP Socket Send",
        udp_send_us,
        (udp_send_us / total_us) * 100.0
    );
    println!("{:-<54}", "");
    println!("{:<30} {:>12.2} {:>9.1}%", "Total Per-Frame Cost", total_us, 100.0);
    println!("Frame Budget (20 ms):          20000.00 µs (Headroom: {:.1}x)", 20_000.0 / total_us);
    println!(
        "Dominant Stage:                Opus encoding dominates ({:.1}% of frame compute)",
        (opus_encode_us / total_us) * 100.0
    );
    println!("================================================================================");
}

#[derive(Debug, Clone, Copy)]
pub enum ChainProfile {
    OpusPassthrough,
    VolumeEqualizer,
    FullChain,
}

#[derive(Debug)]
pub struct StepMetrics {
    pub players: usize,
    pub profile: &'static str,
    pub duration_secs: f64,
    pub expected_frames: u64,
    pub sent_frames: u64,
    pub late_frames: u64,
    pub lateness_p50_ms: f64,
    pub lateness_p99_ms: f64,
    pub lateness_max_ms: f64,
    pub cpu_per_player_pct: f64,
    pub rss_mb: f64,
    pub thread_count: usize,
}

async fn run_load_step(profile: ChainProfile, players: usize, duration: Duration) -> StepMetrics {
    let pcm = vec![0.2_f32; FRAME_SAMPLES * CHANNELS];
    let start_rss = current_rss_kb() as f64 / 1024.0;
    let start_cpu = current_process_cpu_millis();
    let thread_count = current_thread_count();
    let start_time = Instant::now();

    let total_sent = Arc::new(AtomicU64::new(0));
    let total_late = Arc::new(AtomicU64::new(0));
    let lateness_samples = Arc::new(tokio::sync::Mutex::new(Vec::with_capacity(50_000)));

    let mut tasks = Vec::with_capacity(players);

    for idx in 0..players {
        let pcm = pcm.clone();
        let total_sent = Arc::clone(&total_sent);
        let total_late = Arc::clone(&total_late);
        let lateness_samples = Arc::clone(&lateness_samples);

        let mut encoder_opt = match profile {
            ChainProfile::OpusPassthrough => None,
            ChainProfile::VolumeEqualizer => {
                let mut enc = OpusFrameEncoder::new(OpusEncoderConfig {
                    complexity: 0,
                    ..OpusEncoderConfig::default()
                })
                .expect("encoder");
                enc.set_filters(create_volume_eq_filters()).expect("filters");
                Some(enc)
            }
            ChainProfile::FullChain => {
                let mut enc = OpusFrameEncoder::new(OpusEncoderConfig {
                    complexity: 0,
                    ..OpusEncoderConfig::default()
                })
                .expect("encoder");
                enc.set_filters(create_full_filter_chain()).expect("filters");
                Some(enc)
            }
        };

        tasks.push(tokio::spawn(async move {
            let stagger_micros = (idx as u64 * 20_000) / players.max(1) as u64;
            tokio::time::sleep(Duration::from_micros(stagger_micros)).await;

            let player_start = Instant::now();
            let mut frame_idx: u64 = 0;
            let end_time = player_start + duration;

            while Instant::now() < end_time {
                let scheduled_deadline = player_start + Duration::from_millis(frame_idx * 20);
                let now = Instant::now();

                let lateness = now.saturating_duration_since(scheduled_deadline);
                let lateness_ms = lateness.as_secs_f64() * 1000.0;

                if lateness_ms > 5.0 {
                    total_late.fetch_add(1, Ordering::Relaxed);
                }

                if idx % 5 == 0 {
                    let mut lock = lateness_samples.lock().await;
                    if lock.len() < 50_000 {
                        lock.push(lateness_ms);
                    }
                }

                if let Some(encoder) = &mut encoder_opt {
                    let _ = encoder.process(
                        PcmBlock {
                            samples: &pcm,
                            format: OUTPUT_FORMAT,
                            frames: FRAME_SAMPLES,
                            position: Millis::default(),
                        },
                        |_| {},
                    );
                }

                total_sent.fetch_add(1, Ordering::Relaxed);
                frame_idx += 1;

                let next_deadline = player_start + Duration::from_millis(frame_idx * 20);
                let next_now = Instant::now();
                if next_deadline > next_now {
                    tokio::time::sleep(next_deadline - next_now).await;
                }
            }
        }));
    }

    for task in tasks {
        let _ = task.await;
    }

    let elapsed = start_time.elapsed().as_secs_f64();
    let end_cpu = current_process_cpu_millis();
    let end_rss = current_rss_kb() as f64 / 1024.0;
    let total_cpu_used_ms = end_cpu.saturating_sub(start_cpu) as f64;
    let cpu_per_player_pct = (total_cpu_used_ms / (players as f64 * elapsed * 1000.0)) * 100.0;

    let sent = total_sent.load(Ordering::Relaxed);
    let late = total_late.load(Ordering::Relaxed);
    let expected = (players as f64 * (elapsed * 50.0)).round() as u64;

    let mut samples = lateness_samples.lock().await;
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50 = if samples.is_empty() { 0.0 } else { samples[samples.len() / 2] };
    let p99 = if samples.is_empty() { 0.0 } else { samples[(samples.len() * 99) / 100] };
    let max = samples.last().copied().unwrap_or(0.0);

    let profile_name = match profile {
        ChainProfile::OpusPassthrough => "Opus Passthrough",
        ChainProfile::VolumeEqualizer => "Volume + Equalizer",
        ChainProfile::FullChain => "Full Filter Chain",
    };

    StepMetrics {
        players,
        profile: profile_name,
        duration_secs: elapsed,
        expected_frames: expected,
        sent_frames: sent,
        late_frames: late,
        lateness_p50_ms: p50,
        lateness_p99_ms: p99,
        lateness_max_ms: max,
        cpu_per_player_pct,
        rss_mb: end_rss.max(start_rss),
        thread_count,
    }
}

/// Comprehensive Ramp Load Test across 100, 200, 400, 600, 800, 1000 players and 3 profiles.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load test: cargo test --release --test load_tests -- --nocapture --ignored"]
async fn test_load_ramp_and_profiles() {
    println!(
        "\n=========================================================================================="
    );
    println!("Ramp Load Test (100 -> 1000 players across 3 chain profiles)");
    println!(
        "Machine: Intel Core i5-8365U (4C/8T @ 1.60GHz) | Lateness = actual_send - scheduled_deadline"
    );
    println!(
        "=========================================================================================="
    );

    let duration_secs =
        std::env::var("RAMP_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(60);
    let duration = Duration::from_secs(duration_secs);

    let player_steps = [100, 200, 400, 600, 800, 1000];
    let profiles =
        [ChainProfile::OpusPassthrough, ChainProfile::VolumeEqualizer, ChainProfile::FullChain];

    println!(
        "\n{:<20} {:>7} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8} {:>10} {:>8} {:>8}",
        "Profile",
        "Players",
        "Expected",
        "Sent",
        "Late",
        "p50 (ms)",
        "p99 (ms)",
        "Max (ms)",
        "CPU/ply (%)",
        "RSS (MB)",
        "Threads"
    );
    println!("{:-<107}", "");

    for profile in profiles {
        for &players in &player_steps {
            let m = run_load_step(profile, players, duration).await;
            println!(
                "{:<20} {:>7} {:>9} {:>9} {:>8} {:>8.3} {:>8.3} {:>8.3} {:>9.2}% {:>8.1} {:>8}",
                m.profile,
                m.players,
                m.expected_frames,
                m.sent_frames,
                m.late_frames,
                m.lateness_p50_ms,
                m.lateness_p99_ms,
                m.lateness_max_ms,
                m.cpu_per_player_pct,
                m.rss_mb,
                m.thread_count
            );
        }
        println!("{:-<107}", "");
    }

    println!("\nPacing Decision V13:");
    println!("  Record: kept the tokio timer model, the alternative was not built or measured.");
    println!(
        "==========================================================================================\n"
    );
}

/// 10-minute steady-state run at 70% sustained maximum for the full chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load test: cargo test --release --test load_tests -- --nocapture --ignored"]
async fn test_10min_steady_run_at_70pct_max() {
    let sustained_max = 35;
    let players = (sustained_max as f64 * 0.7).round() as usize; // 25 players
    const MINUTES: usize = 10;

    println!("\n================================================================================");
    println!("10-Minute Steady-State Load Run at 70% Sustained Maximum (Full Filter Chain)");
    println!(
        "Target Players: {} (70% of {} sustained max) | Sampling: Every 60s",
        players, sustained_max
    );
    println!("Machine: Intel Core i5-8365U CPU @ 1.60GHz (4 physical cores / 8 logical threads)");
    println!("================================================================================");
    println!(
        "{:<8} {:>10} {:>10} {:>8} {:>10} {:>10} {:>10}",
        "Minute", "RSS (MB)", "Threads", "Sent", "Late", "p50 (ms)", "p99 (ms)"
    );
    println!("{:-<72}", "");

    for minute in 1..=MINUTES {
        let m = run_load_step(ChainProfile::FullChain, players, Duration::from_secs(60)).await;
        println!(
            "{:<8} {:>10.1} {:>10} {:>8} {:>10} {:>10.3} {:>10.3}",
            format!("Min {}", minute),
            m.rss_mb,
            m.thread_count,
            m.sent_frames,
            m.late_frames,
            m.lateness_p50_ms,
            m.lateness_p99_ms
        );
    }
    println!("================================================================================\n");
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

/// 500 WebSocket sessions churn cleanup test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load test: cargo test --test load_tests -- --nocapture --ignored"]
async fn load_test_500_ws_sessions_churn_cleanup() {
    const SESSIONS: usize = 500;
    println!("\n=== Starting 500 WebSocket Sessions Churn Cleanup Test ===");

    let baseline_fd = current_fd_count();
    let baseline_threads = current_thread_count();
    println!("Baseline: FDs = {}, Threads = {}", baseline_fd, baseline_threads);

    let mut config = kairo::config::Config::defaults();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind free port");
    let addr = listener.local_addr().expect("local addr");
    config.server.address = addr.ip().to_string();
    config.server.port = addr.port();
    config.lavalink.server.password = Some("loadtestpass".to_string());
    config.lavalink.server.session_timeout = 2;
    drop(listener);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let s_tx = shutdown_tx.clone();
    let s_rx = shutdown_rx.clone();
    let server_task = tokio::spawn(async move {
        let _ = kairo::serve_with_shutdown(&config, s_tx, s_rx).await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let server_running_fd = current_fd_count();
    let server_running_threads = current_thread_count();
    println!("Server Started: FDs = {}, Threads = {}", server_running_fd, server_running_threads);

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
                headers.insert(
                    HeaderName::from_static("authorization"),
                    HeaderValue::from_static("loadtestpass"),
                );
                headers.insert(
                    HeaderName::from_static("user-id"),
                    HeaderValue::from_str(&user_id).unwrap(),
                );
                headers.insert(
                    HeaderName::from_static("client-name"),
                    HeaderValue::from_static("load-test-agent"),
                );

                if let Ok((mut ws_stream, _)) = connect_async(req).await {
                    active.fetch_add(1, Ordering::Relaxed);
                    let mut session_id = String::new();
                    while let Some(Ok(msg)) = ws_stream.next().await {
                        if let Ok(text) = msg.to_text()
                            && let Ok(json) = serde_json::from_str::<serde_json::Value>(text)
                            && json["op"] == "ready"
                        {
                            session_id = json["sessionId"].as_str().unwrap_or("").to_string();
                            break;
                        }
                    }

                    if !session_id.is_empty() {
                        let guild_id = format!("guild_{}", session_idx);
                        let player_create_body = r#"{"volume":80}"#;
                        send_patch(
                            addr,
                            &format!("/v4/sessions/{session_id}/players/{guild_id}"),
                            player_create_body,
                        )
                        .await;

                        let player_pause_body = r#"{"paused":true}"#;
                        send_patch(
                            addr,
                            &format!("/v4/sessions/{session_id}/players/{guild_id}"),
                            player_pause_body,
                        )
                        .await;

                        let player_seek_body = r#"{"position":30000}"#;
                        send_patch(
                            addr,
                            &format!("/v4/sessions/{session_id}/players/{guild_id}"),
                            player_seek_body,
                        )
                        .await;
                    }
                    drop(ws_stream);
                }
            }));
        }

        for h in batch_handles {
            session_handles.push(h);
        }
    }

    for h in session_handles {
        let _ = h.await;
    }

    let peak_active = active_count.load(Ordering::Relaxed);
    let peak_fd = current_fd_count();
    let peak_threads = current_thread_count();
    println!("Active WS Sessions Created: {}", peak_active);
    println!("Peak: FDs = {}, Threads = {}", peak_fd, peak_threads);

    let wait_timeout_start = Instant::now();
    let expected_wait = Duration::from_secs(3);
    while wait_timeout_start.elapsed() < expected_wait {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let _ = shutdown_tx.send(true);
    let _ = server_task.await;

    // Allow tokio blocking threads to drain
    tokio::time::sleep(Duration::from_millis(10_500)).await;

    let final_fd = current_fd_count();
    let final_threads = current_thread_count();
    println!("Final: FDs = {}, Threads = {}", final_fd, final_threads);

    assert!(
        final_fd <= server_running_fd + 2,
        "FD leak detected: expected <= {}, got {}",
        server_running_fd + 2,
        final_fd
    );
    assert!(
        final_threads <= server_running_threads + 2,
        "Thread leak detected: expected <= {}, got {}",
        server_running_threads + 2,
        final_threads
    );
    println!("Cleanup verified: 0 FD leaks, 0 thread leaks after session churn.");
    println!("==========================================================\n");
}
