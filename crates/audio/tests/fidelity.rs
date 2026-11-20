//! Ignored real-track fidelity render.
//!
//! `KAIRO_FIDELITY_SOURCE=/tmp/opencode/track.m4a cargo test -p kairo-audio --release render_real_pipeline -- --ignored --nocapture`

use std::f64::consts::TAU;
use std::fs::File;
use std::io::{BufWriter, Cursor, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use opus_rs::OpusDecoder;
use tokio::runtime::Handle;

use kairo_core::{AudioFormat, Millis, OpenedSource, SourceHints, SourceReference};

use crate::decode::{BlockingDecoder, PcmBlock};
use crate::limit::Limiter;
use crate::opus::{OpusEncoderConfig, OpusFrameEncoder};
use crate::pcm::{OUTPUT_FORMAT, PcmNormalizer, ResamplingQuality};
use crate::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE};

struct Case {
    name: &'static str,
    bitrate: i32,
    limiter: bool,
    quality: ResamplingQuality,
}

struct Packet {
    bytes: Vec<u8>,
    real_frames: usize,
}

struct Metrics {
    name: &'static str,
    pre_peak: f32,
    pre_over_099: usize,
    post_peak: f32,
    post_over_100: usize,
    frames_below_12db: usize,
    frames: usize,
    overall_snr_db: f64,
    spectral_difference_db: f64,
    payload_bps: f64,
    encode_cpu_percent: f64,
}

#[test]
#[ignore = "decodes and renders the real three-minute track"]
fn render_real_pipeline() {
    let source_path = std::env::var("KAIRO_FIDELITY_SOURCE")
        .unwrap_or_else(|_| String::from("/tmp/opencode/track.m4a"));
    let output_path = std::env::var("KAIRO_FIDELITY_OUTPUT")
        .unwrap_or_else(|_| String::from("/tmp/opencode/audio-fidelity"));
    std::fs::create_dir_all(&output_path).expect("output directory");

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let handle = runtime.handle().clone();
    let results = runtime.block_on(async move {
        tokio::task::spawn_blocking(move || render(&source_path, &output_path, handle))
            .await
            .expect("render worker")
    });

    println!(
        "| case | pre peak | pre >=0.99 | post peak | post >=1.0 | frames <12 dB | frames | overall SNR | spectral delta | payload bit/s | encode CPU |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for result in results {
        println!(
            "| {} | {:.6} | {} | {:.6} | {} | {} | {} | {:.3} dB | {:.3} dB | {:.0} | {:.3}% |",
            result.name,
            result.pre_peak,
            result.pre_over_099,
            result.post_peak,
            result.post_over_100,
            result.frames_below_12db,
            result.frames,
            result.overall_snr_db,
            result.spectral_difference_db,
            result.payload_bps,
            result.encode_cpu_percent,
        );
    }
}

fn render(source_path: &str, output_path: &str, runtime: Handle) -> Vec<Metrics> {
    let (decoded, format) = decode_source(source_path, runtime);
    let high = resample(&decoded, format, ResamplingQuality::High);
    let low = resample(&decoded, format, ResamplingQuality::Low);
    println!(
        "source: {} frames at {} Hz, decoded peak {:.6}, HIGH peak {:.6}",
        decoded.len() / format.channels,
        format.sample_rate,
        peak(&decoded),
        peak(&high),
    );

    let cases = [
        Case {
            name: "99k-no-limiter",
            bitrate: 99_000,
            limiter: false,
            quality: ResamplingQuality::High,
        },
        Case {
            name: "128k-no-limiter",
            bitrate: 128_000,
            limiter: false,
            quality: ResamplingQuality::High,
        },
        Case {
            name: "128k-limiter",
            bitrate: 128_000,
            limiter: true,
            quality: ResamplingQuality::High,
        },
        Case {
            name: "128k-limiter-low",
            bitrate: 128_000,
            limiter: true,
            quality: ResamplingQuality::Low,
        },
    ];

    cases
        .iter()
        .map(|case| {
            let pcm = if case.quality == ResamplingQuality::Low { &low } else { &high };
            let (metrics, rendered) = run_case(case, pcm, &high);
            write_wav(output_path, case.name, &rendered);
            metrics
        })
        .collect()
}

fn decode_source(path: &str, runtime: Handle) -> (Vec<f32>, AudioFormat) {
    let bytes: Arc<[u8]> = std::fs::read(path).expect("source file").into();
    let length = u64::try_from(bytes.len()).expect("source length");
    let source = OpenedSource::seekable(
        "fidelity",
        SourceReference::new(path),
        SourceHints {
            mime_type: Some(String::from("audio/mp4")),
            file_extension: Some(String::from("m4a")),
        },
        Some(length),
        Cursor::new(bytes),
    );
    let mut decoder = BlockingDecoder::open(source, runtime).expect("decoder");
    let mut samples = Vec::new();
    let mut format = None;
    while let Some(block) = decoder.next_block().expect("decode") {
        format = Some(block.format);
        samples.extend_from_slice(block.samples);
    }
    (samples, format.expect("source format"))
}

fn resample(source: &[f32], format: AudioFormat, quality: ResamplingQuality) -> Vec<f32> {
    let mut normalizer = PcmNormalizer::new(quality);
    let mut output = Vec::with_capacity(source.len() * 2);
    let frames = source.len() / format.channels;
    let mut offset = 0;
    while offset < frames {
        let count = (frames - offset).min(4_096);
        normalizer
            .process(
                PcmBlock {
                    samples: &source[offset * format.channels..(offset + count) * format.channels],
                    format,
                    frames: count,
                    position: Millis::default(),
                },
                |block| output.extend_from_slice(block.samples),
            )
            .expect("resample");
        offset += count;
    }
    normalizer.finish(|block| output.extend_from_slice(block.samples)).expect("drain");
    output
}

fn run_case(case: &Case, source: &[f32], reference: &[f32]) -> (Metrics, Vec<f32>) {
    let mut pre_encode = source.to_vec();
    if case.limiter {
        let mut limiter = Limiter::new();
        for samples in pre_encode.chunks_mut(FRAME_SAMPLES * CHANNELS) {
            limiter.process(samples);
        }
    }
    let pre_peak = peak(&pre_encode);
    let pre_over_099 = pre_encode.iter().filter(|sample| sample.abs() >= 0.99).count();

    let mut encoder = OpusFrameEncoder::new(OpusEncoderConfig {
        bitrate_bps: case.bitrate,
        ..OpusEncoderConfig::default()
    })
    .expect("encoder");
    encoder.set_limiter_enabled(case.limiter);
    let mut packets = Vec::new();
    let started = Instant::now();
    let mut offset = 0;
    let frames = source.len() / CHANNELS;
    while offset < frames {
        let count = (frames - offset).min(4_096);
        encoder
            .process(
                PcmBlock {
                    samples: &source[offset * CHANNELS..(offset + count) * CHANNELS],
                    format: OUTPUT_FORMAT,
                    frames: count,
                    position: Millis::default(),
                },
                |frame| {
                    packets.push(Packet {
                        bytes: frame.packet.to_vec(),
                        real_frames: frame.pcm_frames,
                    });
                },
            )
            .expect("encode");
        offset += count;
    }
    encoder
        .finish(|frame| {
            packets.push(Packet { bytes: frame.packet.to_vec(), real_frames: frame.pcm_frames });
        })
        .expect("finish encoder");
    let encode_seconds = started.elapsed().as_secs_f64();

    let rendered = decode_opus(&packets);
    let lag = encoder.lookahead();
    let (frames_below_12db, compared_frames, signal, noise) = frame_snr(reference, &rendered, lag);
    let duration = packets.iter().map(|packet| packet.real_frames).sum::<usize>() as f64
        / f64::from(SAMPLE_RATE);
    let bytes = packets.iter().map(|packet| packet.bytes.len()).sum::<usize>();
    let metrics = Metrics {
        name: case.name,
        pre_peak,
        pre_over_099,
        post_peak: peak(&rendered),
        post_over_100: rendered.iter().filter(|sample| sample.abs() >= 1.0).count(),
        frames_below_12db,
        frames: compared_frames,
        overall_snr_db: 10.0 * (signal / noise.max(1e-30)).log10(),
        spectral_difference_db: spectral_difference(reference, &rendered, lag),
        payload_bps: bytes as f64 * 8.0 / duration,
        encode_cpu_percent: encode_seconds / duration * 100.0,
    };
    (metrics, rendered)
}

fn decode_opus(packets: &[Packet]) -> Vec<f32> {
    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("Opus decoder");
    let mut output = vec![0.0; packets.len() * FRAME_SAMPLES * CHANNELS];
    let mut frames = 0;
    for packet in packets {
        frames += decoder
            .decode(&packet.bytes, FRAME_SAMPLES, &mut output[frames * CHANNELS..])
            .expect("decode Opus");
    }
    output.truncate(frames * CHANNELS);
    output
}

fn frame_snr(source: &[f32], output: &[f32], lag: usize) -> (usize, usize, f64, f64) {
    let frames = (source.len() / CHANNELS).min(output.len() / CHANNELS - lag);
    let (mut below, mut compared) = (0, 0);
    let (mut total_signal, mut total_noise) = (0.0, 0.0);
    for start in (0..frames).step_by(FRAME_SAMPLES) {
        let count = (frames - start).min(FRAME_SAMPLES);
        let (mut signal, mut noise) = (0.0, 0.0);
        for frame in 0..count {
            for channel in 0..CHANNELS {
                let before = f64::from(source[(start + frame) * CHANNELS + channel]);
                let after = f64::from(output[(start + lag + frame) * CHANNELS + channel]);
                signal += before * before;
                noise += (before - after) * (before - after);
            }
        }
        if signal > 1e-20 {
            if 10.0 * (signal / noise.max(1e-30)).log10() < 12.0 {
                below += 1;
            }
            compared += 1;
            total_signal += signal;
            total_noise += noise;
        }
    }
    (below, compared, total_signal, total_noise)
}

fn spectral_difference(source: &[f32], output: &[f32], lag: usize) -> f64 {
    const WINDOW: usize = 4_096;
    const STEP: usize = SAMPLE_RATE as usize * 2;
    const BANDS: usize = 32;
    let frames = (source.len() / CHANNELS).min(output.len() / CHANNELS - lag);
    let mut difference = 0.0;
    let mut measurements = 0;
    for start in (0..frames.saturating_sub(WINDOW)).step_by(STEP) {
        for band in 0..BANDS {
            let ratio = band as f64 / (BANDS - 1) as f64;
            let frequency = 50.0f64 * (20_000.0f64 / 50.0).powf(ratio);
            for channel in 0..CHANNELS {
                let before = magnitude(source, start, channel, frequency, WINDOW);
                if before < 1e-6 {
                    continue;
                }
                let after = magnitude(output, start + lag, channel, frequency, WINDOW);
                difference += (20.0 * ((after + 1e-12) / before).log10()).abs();
                measurements += 1;
            }
        }
    }
    difference / measurements.max(1) as f64
}

fn magnitude(samples: &[f32], start: usize, channel: usize, frequency: f64, frames: usize) -> f64 {
    let step = TAU * frequency / f64::from(SAMPLE_RATE);
    let (mut sine, mut cosine) = (0.0, 0.0);
    for frame in 0..frames {
        let window = 0.5 - 0.5 * (TAU * frame as f64 / (frames - 1) as f64).cos();
        let sample = f64::from(samples[(start + frame) * CHANNELS + channel]) * window;
        let angle = step * (start + frame) as f64;
        sine += sample * angle.sin();
        cosine += sample * angle.cos();
    }
    2.0 * sine.hypot(cosine) / frames as f64
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

fn write_wav(directory: &str, name: &str, samples: &[f32]) {
    let path = Path::new(directory).join(format!("{name}.wav"));
    let mut writer = BufWriter::new(File::create(&path).expect("WAV file"));
    let data_len = u32::try_from(size_of_val(samples)).expect("WAV data length");
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * size_of::<f32>() as u32;
    writer.write_all(b"RIFF").expect("RIFF");
    writer.write_all(&(36 + data_len).to_le_bytes()).expect("RIFF length");
    writer.write_all(b"WAVEfmt ").expect("WAVE");
    writer.write_all(&16u32.to_le_bytes()).expect("fmt size");
    writer.write_all(&3u16.to_le_bytes()).expect("float format");
    writer.write_all(&(CHANNELS as u16).to_le_bytes()).expect("channels");
    writer.write_all(&SAMPLE_RATE.to_le_bytes()).expect("sample rate");
    writer.write_all(&byte_rate.to_le_bytes()).expect("byte rate");
    writer.write_all(&((CHANNELS * size_of::<f32>()) as u16).to_le_bytes()).expect("align");
    writer.write_all(&32u16.to_le_bytes()).expect("sample bits");
    writer.write_all(b"data").expect("data");
    writer.write_all(&data_len.to_le_bytes()).expect("data length");
    for sample in samples {
        writer.write_all(&sample.to_le_bytes()).expect("sample");
    }
    println!("wrote {}", path.display());
}
