//! AAC access-unit decoding through `syom`.

use symphonia::core::codecs::audio::AudioCodecParameters;

use kairo_core::AudioFormat;

use super::DecodeError;

/// The AAC-LC object type, the only profile an ADTS header can name.
const AOT_LC: u8 = 2;

/// One packet's worth of native-rate PCM, laid out interleaved in [`AacDecoder::samples`].
pub(super) struct AacFrames {
    pub(super) format: AudioFormat,
    pub(super) frames: usize,
}

/// A push decoder configured from an `AudioSpecificConfig`.
pub(super) struct AacDecoder {
    decoder: syom::Decoder,
    config: Vec<u8>,
}

impl AacDecoder {
    /// Build a decoder from the container's codec parameters.
    pub(super) fn open(params: &AudioCodecParameters) -> Option<Self> {
        let config = match params.extra_data.as_deref() {
            Some(config) if !config.is_empty() => config.to_vec(),
            _ => lc_config(params.sample_rate?, params.channels.as_ref()?.count())?,
        };
        let decoder = syom::Decoder::from_asc(&config, options()).ok()?;
        Some(Self { decoder, config })
    }

    /// Decode one complete access unit into `samples`, interleaved.
    pub(super) fn decode(
        &mut self,
        packet: &[u8],
        samples: &mut Vec<f32>,
    ) -> Result<Option<AacFrames>, DecodeError> {
        let mut rate = 0;
        let mut channels = 0;
        let mut frames = 0;
        samples.clear();
        let decoded = self.decoder.decode_au(packet, |frame| {
            let frame_channels = frame.planar.len();
            if channels == 0 {
                rate = frame.sample_rate;
                channels = frame_channels;
            } else if rate != frame.sample_rate || channels != frame_channels {
                return Err(syom::AacError::decode(
                    "aac: sample rate or channel count changed inside one access unit",
                ));
            }
            interleave(frame.planar, frame.samples, samples);
            frames += frame.samples;
            Ok(())
        });
        match decoded {
            Ok(()) => {
                let format =
                    AudioFormat::new(rate, channels).ok_or(DecodeError::InvalidAudioFormat)?;
                Ok(Some(AacFrames { format, frames }))
            }
            Err(error) if is_packet_error(&error) => {
                self.restart()?;
                Ok(None)
            }
            Err(error) => {
                self.restart()?;
                Err(DecodeError::Aac { message: error.to_string() })
            }
        }
    }

    /// Rebuild the decoder after an error.
    fn restart(&mut self) -> Result<(), DecodeError> {
        self.decoder = syom::Decoder::from_asc(&self.config, options())
            .map_err(|error| DecodeError::Aac { message: error.to_string() })?;
        Ok(())
    }
}

/// Split channels, no artificial track-length ceiling.
fn options() -> syom::DecodeOptions {
    syom::DecodeOptions::audio().with_max_duration_secs(f64::INFINITY)
}

/// Copy planar planes into an interleaved buffer.
fn interleave(planes: &[&[f32]], frames: usize, samples: &mut Vec<f32>) {
    if planes.len() == 1 {
        samples.extend_from_slice(&planes[0][..frames]);
        return;
    }
    if let [left, right] = planes {
        samples.reserve(frames * 2);
        let common = frames.min(left.len()).min(right.len());
        for (&l, &r) in left[..common].iter().zip(&right[..common]) {
            samples.push(l);
            samples.push(r);
        }
        for index in common..frames {
            samples.push(left.get(index).copied().unwrap_or(0.0));
            samples.push(right.get(index).copied().unwrap_or(0.0));
        }
        return;
    }
    samples.reserve(frames * planes.len());
    for index in 0..frames {
        for plane in planes {
            samples.push(plane.get(index).copied().unwrap_or(0.0));
        }
    }
}

/// Check if an AAC error is a packet-level error.
fn is_packet_error(error: &syom::AacError) -> bool {
    matches!(
        error,
        syom::AacError::Malformed(_)
            | syom::AacError::Truncated { .. }
            | syom::AacError::NeedMore { .. }
            | syom::AacError::Decode(_)
            | syom::AacError::Format(_)
    )
}

/// The standard two-byte `AudioSpecificConfig` for AAC-LC.
fn lc_config(sample_rate: u32, channels: usize) -> Option<Vec<u8>> {
    let rate_index: u8 = match sample_rate {
        96_000 => 0,
        88_200 => 1,
        64_000 => 2,
        48_000 => 3,
        44_100 => 4,
        32_000 => 5,
        24_000 => 6,
        22_050 => 7,
        16_000 => 8,
        12_000 => 9,
        11_025 => 10,
        8_000 => 11,
        7_350 => 12,
        _ => return None,
    };
    if !(1..=7).contains(&channels) {
        return None;
    }
    let config =
        (u16::from(AOT_LC) << 11) | (u16::from(rate_index) << 7) | ((channels as u16) << 3);
    Some(config.to_be_bytes().to_vec())
}

#[cfg(test)]
#[path = "../../tests/decode_aac.rs"]
mod tests;
