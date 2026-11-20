//! Ignored end-to-end fidelity render used by the audio fidelity report.
//!
//! Run with the real track available:
//! `KAIRO_FIDELITY_SOURCE=/tmp/opencode/track.m4a cargo test -p kairo-audio --release fidelity -- --ignored --nocapture`

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use opus_rs::{Application, OpusDecoder, OpusEncoder};
use tokio::runtime::Handle;

use kairo_core::{OpenedSource, SourceHints, SourceReference};

use crate::decode::BlockingDecoder;
use crate::limit::Limiter;
use crate::pcm::{PcmNormalizer, ResamplingQuality};
use crate::{CHANNELS, FRAME_SAMPLES, MAX_PACKET_BYTES, SAMPLE_RATE};

const BITRATES: [i32; 4] = [96_000, 128_000, 192_000, 256_000];

struct Render {
    name: &'static str,
    bitrate: i32,
    limiter: bool,
    quality: ResamplingQuality,
}

struct Measured {
    name: &'static str,
    bitrate: i32,
    pre_peak: f32,
    pre_over_099: usize,
    post_peak: f32,
    post_over_100: usize,
    frames_below_12db: usize,
    frames: usize,
    overall_snr_db: f64,
    encoded_bits_per_second: f64,
    cpu_percent: f64,
}

#[test]
#[ignore = "renders the real track: run with --release --ignored"]
fn render_fidelity_cases() {
    let source_path = std::env::var("KAIRO_FIDELITY_SOURCE")
        .unwrap_or_else(|_| String::from("/tmp/opencode/track.m4a"));
    let output = std::env::var("KAIRO_FIDELITY_OUTPUT")
        .unwrap_or_else(|_| String::from("/tmp/opencode/fidelity"));
    std::fs::create_dir_all(&output).expect("output directory");

    let (source, format) = decode_source(&source_path);
    println!(
        "source: {} frames at {} Hz, {} ch, peak {:.4}",
        source.len() / CHANNELS,
        format.sample_rate,
        format.channels,
        peak(&source)
    );

    let cases = [
        Render {
            name: "99k-no-limiter",
            bitrate: 99_000,
            limiter: false,
            quality: ResamplingQuality::High,
        },
        Render {
            name: "128k-no-limiter",
            bitrate: 128_000,
            limiter: false,
            quality: ResamplingQuality::High,
        },
        Render {
            name: "128k-limiter",
            bitrate: 128_000,
            limiter: true,
            quality: ResamplingQuality::High,
        },
        Render {
            name: "128k-limiter-low",
            bitrate: 128_000,
            limiter: true,
            quality: ResamplingQuality::Low,
        },
    ];

    let mut results = Vec::new();
    for case in &cases {
        let resampled = resample(&source, format, case.quality);
        let (measured, decoded) = encode_decode(resampled, case);
        write_wav(&output, case.name, &decoded);
        results.push(measured);
    }

    for bitrate in BITRATES {
        let case =
            Render { name: "bitrate", bitrate, limiter: false, quality: ResamplingQuality::High };
        let resampled = resample(&source, format, case.quality);
        results.push(encode_decode(resampled, &case).0);
    }

    println!(
        "\n| case | bitrate | pre peak | pre >=0.99 | post peak | post >=1.0 | frames <12 dB | frames | overall SNR | wire bit/s | CPU % |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for result in &results {
        println!(
            "| {} | {}k | {:.4} | {} | {:.4} | {} | {} | {} | {:.1} dB | {:.0} | {:.2} |",
            result.name,
            result.bitrate / 1_000,
            result.pre_peak,
            result.pre_over_099,
            result.post_peak,
            result.post_over_100,
            result.frames_below_12db,
            result.frames,
            result.overall_snr_db,
            result.encoded_bits_per_second,
            result.cpu_percent,
        );
    }
}

fn decode_source(path: &str) -> (Vec<f32>, kairo_core::AudioFormat) {
    let bytes = std::fs::read(path).expect("source file");
    let length = u64::try_from(bytes.len()).expect("length");
    let runtime = Handle::current();
    let source = OpenedSource::seekable(
        "fidelity",
        SourceReference::new(path),
        SourceHints::default(),
        Some(length),
        std::io::Cursor::new(bytes),
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

fn resample(
    source: &[f32],
    format: kairo_core::AudioFormat,
    quality: ResamplingQuality,
) -> Vec<f32> {
    let mut normalizer = PcmNormalizer::new(quality);
    let mut output = Vec::with_capacity(source.len() * 2);
    let frames = source.len() / CHANNELS;
    let mut offset = 0;
    while offset < frames {
        let count = (frames - offset).min(4_096);
        normalizer
            .process(
                crate::decode::PcmBlock {
                    samples: &source[offset * CHANNELS..(offset + count) * CHANNELS],
                    format,
                    frames: count,
                    position: kairo_core::Millis::new(0),
                },
                |block| output.extend_from_slice(block.samples),
            )
            .expect("resample");
        offset += count;
    }
    normalizer.finish(|block| output.extend_from_slice(block.samples)).expect("drain");
    output
}

fn encode_decode(mut pcm: Vec<f32>, case: &Render) -> (Measured, Vec<f32>) {
    let frames = pcm.len() / CHANNELS;
    let padded = frames.div_ceil(FRAME_SAMPLES) * FRAME_SAMPLES;
    pcm.resize(padded * CHANNELS, 0.0);
    assert_eq!(pcm.len(), padded * CHANNELS);

    let mut limiter = Limiter::new();
    if case.limiter {
        for frame in pcm.chunks_mut(FRAME_SAMPLES * CHANNELS) {
            limiter.process(frame);
        }
    }
    let pre_peak = peak(&pcm);
    let pre_over_099 = pcm.iter().filter(|sample| sample.abs() >= 0.99).count();

    let mut encoder =
        OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio).expect("encoder");
    encoder.bitrate_bps = case.bitrate;
    encoder.complexity = 10;
    let mut packet = [0u8; MAX_PACKET_BYTES];
    let mut packets = Vec::new();
    let mut encoded_bytes = 0u64;
    let start = Instant::now();
    for frame in pcm.chunks(FRAME_SAMPLES * CHANNELS) {
        let length = encoder.encode(frame, FRAME_SAMPLES, &mut packet).expect("encode");
        encoded_bytes += u64::try_from(length).expect("packet length");
        packets.push(packet[..length].to_vec());
    }
    let elapsed = start.elapsed().as_secs_f64();

    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder");
    let mut decoded = vec![0.0f32; pcm.len()];
    let mut decoded_frames = 0;
    for packet in &packets {
        let count = decoder
            .decode(packet, FRAME_SAMPLES, &mut decoded[decoded_frames * CHANNELS..])
            .expect("decode");
        decoded_frames += count;
    }
    decoded.truncate(decoded_frames * CHANNELS);

    let lag = best_lag(&pcm, &decoded, SAMPLE_RATE as usize * 10);
    let (frames_below, frames_compared, snr) = frame_snr(&pcm, &decoded, lag);
    let duration = padded as f64 / SAMPLE_RATE as f64;

    let measured = Measured {
        name: case.name,
        bitrate: case.bitrate,
        pre_peak,
        pre_over_099,
        post_peak: peak(&decoded),
        post_over_100: decoded.iter().filter(|sample| sample.abs() >= 1.0).count(),
        frames_below_12db: frames_below,
        frames: frames_compared,
        overall_snr_db: snr,
        encoded_bits_per_second: encoded_bytes as f64 * 8.0 / duration,
        cpu_percent: elapsed / duration * 100.0,
    };
    (measured, decoded)
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

/// Integer lag that best aligns the decoder output to the encoder input.
fn best_lag(pre: &[f32], decoded: &[f32], compare: usize) -> isize {
    let limit = pre.len().min(decoded.len()).min(compare);
    let mut best = (f64::MAX, 0isize);
    for lag in -600isize..=600 {
        let mut error = 0.0f64;
        for index in (0..limit).step_by(4) {
            let target = index as isize + lag;
            if target < 0 || target as usize >= decoded.len() {
                continue;
            }
            let difference = f64::from(pre[index] - decoded[target as usize]);
            error += difference * difference;
        }
        if error < best.0 {
            best = (error, lag);
        }
    }
    best.1
}

/// Frame SNR against the pre-encode signal, after removing the codec alignment lag.
fn frame_snr(pre: &[f32], decoded: &[f32], lag: isize) -> (usize, usize, f64) {
    let mut below = 0;
    let mut compared = 0;
    let mut signal = 0.0f64;
    let mut noise = 0.0f64;
    let step = FRAME_SAMPLES * CHANNELS;
    let mut index = 0;
    while index + step <= pre.len() {
        let target = index as isize + lag;
        if target < 0 || target as usize + step > decoded.len() {
            index += step;
            continue;
        }
        let window = &decoded[target as usize..target as usize + step];
        let mut frame_signal = 0.0f64;
        let mut frame_noise = 0.0f64;
        for (&before, &after) in pre[index..index + step].iter().zip(window) {
            frame_signal += f64::from(before) * f64::from(before);
            let difference = f64::from(before - after);
            frame_noise += difference * difference;
        }
        signal += frame_signal;
        noise += frame_noise;
        if frame_signal > 0.0 {
            let snr = 10.0 * (frame_signal / frame_noise.max(1e-30)).log10();
            if snr < 12.0 {
                below += 1;
            }
            compared += 1;
        }
        index += step;
    }
    (below, compared, 10.0 * (signal / noise.max(1e-30)).log10())
}

fn write_wav(directory: &str, name: &str, samples: &[f32]) {
    let path = Path::new(directory).join(format!("{name}.wav"));
    let mut writer = BufWriter::new(File::create(&path).expect("wav file"));
    let data_len = u32::try_from(samples.len() * 4).expect("wav data length");
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * 4;
    writer.write_all(b"RIFF").expect("riff");
    writer.write_all(&(36 + data_len).to_le_bytes()).expect("riff length");
    writer.write_all(b"WAVEfmt ").expect("wave");
    writer.write_all(&16u32.to_le_bytes()).expect("fmt size");
    writer.write_all(&3u16.to_le_bytes()).expect("float format");
    writer.write_all(&(CHANNELS as u16).to_le_bytes()).expect("channels");
    writer.write_all(&SAMPLE_RATE.to_le_bytes()).expect("sample rate");
    writer.write_all(&byte_rate.to_le_bytes()).expect("byte rate");
    writer.write_all(&((CHANNELS * 4) as u16).to_le_bytes()).expect("block align");
    writer.write_all(&32u16.to_le_bytes()).expect("bits");
    writer.write_all(b"data").expect("data");
    writer.write_all(&data_len.to_le_bytes()).expect("data length");
    for sample in samples {
        writer.write_all(&sample.to_le_bytes()).expect("sample");
    }
    println!("wrote {}", path.display());
}

#[test]
#[ignore = "measures the resampler: run with --release --ignored"]
fn resampler_transparency_44k1_to_48k() {
    let frequencies = [20.0, 100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 18_000.0, 20_000.0];
    let mut gains = Vec::new();
    for frequency in frequencies {
        let output = resample_tone(frequency);
        let gain = 20.0 * (magnitude_at(&output, frequency) / 0.5).log10();
        gains.push(gain);
        println!("passband {frequency:>7.0} Hz: {gain:+.4} dB");
    }
    let ripple = gains.iter().cloned().fold(f64::MIN, f64::max)
        - gains.iter().cloned().fold(f64::MAX, f64::min);
    println!("passband ripple 20 Hz..20 kHz: {ripple:.4} dB");

    let output = resample_tone(21_000.0);
    let tone = magnitude_at(&output, 21_000.0);
    let image = magnitude_at(&output, 23_100.0);
    println!("21 kHz image at 23.1 kHz: {:.1} dB below the tone", 20.0 * (tone / image).log10());

    let output = resample_tone(1_000.0);
    println!("1 kHz THD+N: {:.1} dB", thd_plus_noise(&output, 1_000.0));
}

fn resample_tone(frequency: f64) -> Vec<f32> {
    let input_rate = 44_100u32;
    let frames = input_rate as usize * 3;
    let mut input = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        let sample =
            (std::f64::consts::TAU * frequency * frame as f64 / f64::from(input_rate)).sin() * 0.5;
        input.extend_from_slice(&[sample as f32, sample as f32]);
    }
    let format = kairo_core::AudioFormat { sample_rate: input_rate, channels: CHANNELS };
    let mut normalizer = PcmNormalizer::new(ResamplingQuality::High);
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < frames {
        let count = (frames - offset).min(1_024);
        normalizer
            .process(
                crate::decode::PcmBlock {
                    samples: &input[offset * CHANNELS..(offset + count) * CHANNELS],
                    format,
                    frames: count,
                    position: kairo_core::Millis::new(offset as u64 * 1_000 / 44),
                },
                |block| output.extend_from_slice(block.samples),
            )
            .expect("resample");
        offset += count;
    }
    normalizer.finish(|block| output.extend_from_slice(block.samples)).expect("drain");
    // Skip the converter warm-up and measure one steady second.
    let skip = SAMPLE_RATE as usize / 2;
    output[skip * CHANNELS..(skip + SAMPLE_RATE as usize) * CHANNELS].to_vec()
}

fn magnitude_at(samples: &[f32], frequency: f64) -> f64 {
    let frames = samples.len() / CHANNELS;
    let step = std::f64::consts::TAU * frequency / f64::from(SAMPLE_RATE);
    let (mut real, mut imaginary) = (0.0f64, 0.0f64);
    for frame in 0..frames {
        let angle = step * frame as f64;
        let sample = f64::from(samples[frame * CHANNELS]);
        real += sample * angle.cos();
        imaginary += sample * angle.sin();
    }
    2.0 * real.hypot(imaginary) / frames as f64
}

fn thd_plus_noise(samples: &[f32], frequency: f64) -> f64 {
    let frames = samples.len() / CHANNELS;
    let step = std::f64::consts::TAU * frequency / f64::from(SAMPLE_RATE);
    let (mut sine, mut cosine) = (0.0f64, 0.0f64);
    for frame in 0..frames {
        let angle = step * frame as f64;
        let sample = f64::from(samples[frame * CHANNELS]);
        sine += sample * angle.sin();
        cosine += sample * angle.cos();
    }
    let scale = 2.0 / frames as f64;
    let (a, b) = (sine * scale, cosine * scale);
    let mut residual = 0.0f64;
    for frame in 0..frames {
        let angle = step * frame as f64;
        let model = a * angle.sin() + b * angle.cos();
        let difference = f64::from(samples[frame * CHANNELS]) - model;
        residual += difference * difference;
    }
    let residual_rms = (residual / frames as f64).sqrt();
    let fundamental_rms = a.hypot(b) / std::f64::consts::SQRT_2;
    20.0 * (residual_rms / fundamental_rms).log10()
}
