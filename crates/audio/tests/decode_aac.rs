use std::io::Cursor;

use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_AAC;
use tokio::runtime::Handle;

use kairo_core::{OpenedSource, SourceHints, SourceReference};

use super::super::{AudioFormat, BlockingDecoder, DecodeError};
use super::{AacDecoder, lc_config};

fn source(bytes: Vec<u8>, seekable: bool) -> OpenedSource {
    let length = bytes.len() as u64;
    if seekable {
        OpenedSource::seekable(
            "memory",
            SourceReference::new("fixture"),
            SourceHints::default(),
            Some(length),
            Cursor::new(bytes),
        )
    } else {
        OpenedSource::read_only(
            "memory",
            SourceReference::new("fixture"),
            SourceHints::default(),
            Some(length),
            Cursor::new(bytes),
        )
    }
}

struct Decoded {
    format: Option<AudioFormat>,
    frames: usize,
}

async fn decode(bytes: Vec<u8>) -> Result<Decoded, DecodeError> {
    let runtime = Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(source(bytes, true), runtime)?;
        let mut decoded = Decoded { format: None, frames: 0 };
        while let Some(block) = decoder.next_block()? {
            decoded.format = Some(block.format);
            decoded.frames += block.frames;
        }
        Ok(decoded)
    })
    .await
    .expect("decode worker")
}

fn tone(sample_rate: u32, channels: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|channel| {
            let offset = channel as f32 * 0.7;
            (0..frames)
                .map(|index| {
                    let phase = index as f32 * 0.0576 * sample_rate as f32 / 48_000.0;
                    0.3 * (phase + offset).sin()
                })
                .collect()
        })
        .collect()
}

/// Drop the leading ID3 `iTunSMPB` tag the encoder prepends to ADTS.
fn strip_id3(bytes: &[u8]) -> &[u8] {
    if bytes.len() < 10 || &bytes[..3] != b"ID3" {
        return bytes;
    }
    let size = (u32::from(bytes[6] & 0x7f) << 21)
        | (u32::from(bytes[7] & 0x7f) << 14)
        | (u32::from(bytes[8] & 0x7f) << 7)
        | u32::from(bytes[9] & 0x7f);
    let total = 10 + usize::try_from(size).expect("ID3 size");
    bytes.get(total..).unwrap_or_default()
}

#[test]
fn the_lc_config_is_the_standard_two_byte_layout() {
    assert_eq!(lc_config(48_000, 1).expect("mono config"), vec![0x11, 0x88]);
    assert_eq!(lc_config(48_000, 2).expect("stereo config"), vec![0x11, 0x90]);
    assert_eq!(lc_config(44_100, 2).expect("44.1 kHz config"), vec![0x12, 0x10]);
    assert!(lc_config(48_123, 2).is_none());
    assert!(lc_config(48_000, 8).is_none());
}

#[tokio::test]
async fn he_aac_v1_adts_decodes_at_the_sbr_output_rate() {
    let pcm = tone(48_000, 2, 48_000);
    let options = syom::EncodeOptions::adts().with_he(true).with_bitrate_bps(48_000);
    let adts = syom::encode_with(&pcm, 48_000, &options).expect("HE-AAC v1 encode");
    let decoded = decode(strip_id3(&adts).to_vec()).await.expect("HE-AAC v1 decode");

    let format = decoded.format.expect("decoded format");
    assert_eq!(format.sample_rate, 48_000);
    assert_eq!(format.channels, 2);
    assert!(decoded.frames > 48_000, "one second of HE-AAC decoded as {} frames", decoded.frames);
}

#[tokio::test]
async fn he_aac_v2_adts_expands_a_mono_core_to_stereo() {
    let pcm = tone(48_000, 2, 48_000);
    let options = syom::EncodeOptions::adts().with_he_v2(true).with_bitrate_bps(32_000);
    let adts = syom::encode_with(&pcm, 48_000, &options).expect("HE-AAC v2 encode");
    let decoded = decode(strip_id3(&adts).to_vec()).await.expect("HE-AAC v2 decode");

    let format = decoded.format.expect("decoded format");
    assert_eq!(format.sample_rate, 48_000);
    assert_eq!(format.channels, 2, "parametric stereo must present two channels");
    assert!(decoded.frames > 48_000, "one second of HE-AAC decoded as {} frames", decoded.frames);
}

/// The explicit two-rate HE-AAC v1 `AudioSpecificConfig` an ISO-MP4 `esds`
/// carries: outer AOT 5, core rate, channel config, output rate, inner LC.
fn he_asc(core_index: u8, output_index: u8, channels: u8) -> Vec<u8> {
    let bits = (5u32 << 27)
        | (u32::from(core_index) << 23)
        | (u32::from(channels) << 19)
        | (u32::from(output_index) << 15)
        | (2u32 << 10);
    bits.to_be_bytes().to_vec()
}

/// Split an ADTS stream into raw `raw_data_block()` payloads, which is what
/// an ISO-MP4 sample table stores.
fn adts_payloads(bytes: &[u8]) -> Vec<&[u8]> {
    let mut payloads = Vec::new();
    let mut position = 0;
    while position + 7 <= bytes.len() {
        if bytes[position] != 0xff || bytes[position + 1] & 0xf6 != 0xf0 {
            break;
        }
        let length = (usize::from(bytes[position + 3] & 0x03) << 11)
            | (usize::from(bytes[position + 4]) << 3)
            | (usize::from(bytes[position + 5]) >> 5);
        if length < 7 || position + length > bytes.len() {
            break;
        }
        let header = if bytes[position + 1] & 0x01 == 1 { 7 } else { 9 };
        if header < length {
            payloads.push(&bytes[position + header..position + length]);
        }
        position += length;
    }
    payloads
}

#[test]
fn an_explicit_he_config_decodes_raw_access_units() {
    let pcm = tone(48_000, 2, 48_000);
    let options = syom::EncodeOptions::adts().with_he(true).with_bitrate_bps(48_000);
    let adts = syom::encode_with(&pcm, 48_000, &options).expect("HE-AAC v1 encode");

    // ISO-MP4 supplies the config out of band instead of an ADTS header.
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_AAC);
    params.extra_data = Some(he_asc(6, 3, 2).into_boxed_slice());
    let mut decoder = AacDecoder::open(&params).expect("explicit HE config");

    let mut samples = Vec::new();
    let mut format = None;
    let mut frames = 0;
    for payload in adts_payloads(strip_id3(&adts)) {
        if let Some(decoded) = decoder.decode(payload, &mut samples).expect("decode access unit") {
            format = Some(decoded.format);
            frames += decoded.frames;
        }
    }

    let format = format.expect("decoded format");
    assert_eq!(format.sample_rate, 48_000);
    assert_eq!(format.channels, 2);
    assert!(frames > 48_000, "one second of HE-AAC decoded as {frames} frames");
}

#[test]
fn stereo_interleave_zips_planar_channels() {
    let left = [1.0, 2.0, 3.0];
    let right = [4.0, 5.0, 6.0];
    let mut samples = Vec::new();
    super::interleave(&[&left[..], &right[..]], 3, &mut samples);
    assert_eq!(samples, [1.0, 4.0, 2.0, 5.0, 3.0, 6.0]);

    let mut padded = Vec::new();
    super::interleave(&[&left[..2], &right[..1]], 3, &mut padded);
    assert_eq!(padded, [1.0, 4.0, 2.0, 0.0, 0.0, 0.0]);
}
