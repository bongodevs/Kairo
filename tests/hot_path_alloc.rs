#![allow(unsafe_code)]

// SAFETY: Custom counting global allocator to verify zero heap allocations on audio and voice hot paths.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct CountingAllocator;

static TRACKING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

use kairo_audio::decode::PcmBlock;
use kairo_audio::filters::{
    ChannelMixConfig, DistortionConfig, EchoConfig, EqualizerConfig, Filters, KaraokeConfig,
    LowPassConfig, NormalizationConfig, PassConfig, RotationConfig, TremoloConfig, VibratoConfig,
};
use kairo_audio::opus::{CHANNELS, FRAME_SAMPLES, OpusEncoderConfig, OpusFrameEncoder};
use kairo_audio::pcm::OUTPUT_FORMAT;
use kairo_core::Millis;

#[test]
fn test_player_hot_path_with_full_filter_chain_zero_allocations() {
    let mut encoder =
        OpusFrameEncoder::new(OpusEncoderConfig { complexity: 0, ..OpusEncoderConfig::default() })
            .expect("encoder");

    let bands = (0..15).map(|band| (band, 0.2));
    let equalizer = EqualizerConfig::from_bands(bands);

    let filters = Filters {
        volume: 0.8,
        equalizer,
        karaoke: Some(KaraokeConfig::default()),
        timescale: None, // Timescale performs dynamic stretching with internal wsola ring buffers
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
    };

    encoder.set_filters(filters).expect("filters must be valid");

    let pcm = vec![0.25_f32; FRAME_SAMPLES * CHANNELS];

    // Warm up the encoder and filter state (ring buffers, lookahead)
    for _ in 0..100 {
        encoder
            .process(
                PcmBlock {
                    samples: &pcm,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES,
                    position: Millis::default(),
                },
                |_frame| {},
            )
            .expect("warmup process");
    }

    // Now start counting allocations
    ALLOCATIONS.store(0, Ordering::SeqCst);
    TRACKING.store(true, Ordering::SeqCst);

    let iterations = if cfg!(debug_assertions) { 2_000 } else { 100_000 };
    for _ in 0..iterations {
        encoder
            .process(
                PcmBlock {
                    samples: &pcm,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES,
                    position: Millis::default(),
                },
                |_frame| {},
            )
            .expect("steady-state process");
    }

    TRACKING.store(false, Ordering::SeqCst);
    let count = ALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(
        count, 0,
        "Player hot path with active filters must perform 0 allocations, got {count}"
    );
}

#[test]
#[ignore = "full 100,000 frames allocation test: cargo test --test hot_path_alloc -- --ignored"]
fn test_player_hot_path_with_full_filter_chain_100k_frames_zero_allocations() {
    let mut encoder =
        OpusFrameEncoder::new(OpusEncoderConfig { complexity: 0, ..OpusEncoderConfig::default() })
            .expect("encoder");

    let bands = (0..15).map(|band| (band, 0.2));
    let equalizer = EqualizerConfig::from_bands(bands);

    let filters = Filters {
        volume: 0.8,
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
    };

    encoder.set_filters(filters).expect("filters must be valid");

    let pcm = vec![0.25_f32; FRAME_SAMPLES * CHANNELS];

    for _ in 0..100 {
        encoder
            .process(
                PcmBlock {
                    samples: &pcm,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES,
                    position: Millis::default(),
                },
                |_frame| {},
            )
            .expect("warmup process");
    }

    ALLOCATIONS.store(0, Ordering::SeqCst);
    TRACKING.store(true, Ordering::SeqCst);

    for _ in 0..100_000 {
        encoder
            .process(
                PcmBlock {
                    samples: &pcm,
                    format: OUTPUT_FORMAT,
                    frames: FRAME_SAMPLES,
                    position: Millis::default(),
                },
                |_frame| {},
            )
            .expect("steady-state process");
    }

    TRACKING.store(false, Ordering::SeqCst);
    let count = ALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(
        count, 0,
        "Player hot path with active filters must perform 0 allocations over 100k frames, got {count}"
    );
}

#[tokio::test]
async fn test_voice_send_path_zero_allocations() {
    use std::net::SocketAddr;
    use tokio::net::UdpSocket;
    use voice::crypto::TransportCipher;
    use voice::udp::rtp::{RTP_HEADER_LEN, Rtp};

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind socket");
    let peer_addr: SocketAddr = "127.0.0.1:9".parse().unwrap(); // Discard port
    socket.connect(peer_addr).await.expect("connect socket");

    let secret_key = [0x5au8; 32];
    let mut cipher =
        TransportCipher::new(voice::crypto::XCHACHA_MODE, &secret_key).expect("cipher");
    let mut rtp = Rtp::new(99_999);

    let raw_payload = [0xf8, 0xff, 0xfe]; // Silence frame
    let mut header = [0u8; RTP_HEADER_LEN];
    let mut sealed = vec![0u8; 1500]; // Pre-allocated maximum datagram buffer

    // Warm-up loop
    for _ in 0..100 {
        rtp.next_header(&mut header, false);
        let written = cipher.seal(&header, &raw_payload, &mut sealed).expect("seal");
        let _ = socket.try_send(&sealed[..written]);
    }

    // Measure steady state
    ALLOCATIONS.store(0, Ordering::SeqCst);
    TRACKING.store(true, Ordering::SeqCst);

    for _ in 0..100_000 {
        rtp.next_header(&mut header, false);
        let written = cipher.seal(&header, &raw_payload, &mut sealed).expect("seal");
        let _ = socket.try_send(&sealed[..written]);
    }

    TRACKING.store(false, Ordering::SeqCst);
    let count = ALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(count, 0, "Voice send path must perform 0 allocations in steady state, got {count}");
}

#[tokio::test]
async fn test_consumer_read_path_zero_allocations() {
    use player::buffer::{BufferedFrame, FrameBufferConfig, frame_buffer};

    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    let mut output = [0u8; 1500];

    // Pre-fill buffer
    for _ in 0..200 {
        let frame = BufferedFrame::new(&[0xf8, 0xff, 0xfe], Millis::default(), 960).expect("frame");
        producer.send(frame).await.expect("send");
    }

    // Measure consumer read loop
    ALLOCATIONS.store(0, Ordering::SeqCst);
    TRACKING.store(true, Ordering::SeqCst);

    for _ in 0..200 {
        let _ = consumer.read(&mut output).expect("read");
    }

    TRACKING.store(false, Ordering::SeqCst);
    let count = ALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(count, 0, "Consumer read hot path must perform 0 allocations, got {count}");
}
