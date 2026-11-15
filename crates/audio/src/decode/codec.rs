use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioCodecId;
use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_VORBIS,
};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::{FormatReader, TrackType};
use symphonia::core::units::{Duration, TimeBase, Timestamp};

use kairo_core::{AudioFormat, Millis};

use super::DecodeError;
use super::aac::AacDecoder;
use crate::SAMPLE_RATE;

pub(super) enum DecoderKind {
    Media(Box<dyn AudioDecoder>),
    Aac(Box<AacDecoder>),
    Opus { decoder: Box<opus_rs::OpusDecoder>, channels: usize },
}

impl DecoderKind {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Media(decoder) => decoder.codec_info().short_name,
            Self::Aac(_) => "aac",
            Self::Opus { .. } => "opus",
        }
    }

    pub(super) fn finalize(&mut self) {
        if let Self::Media(decoder) = self {
            let _ = decoder.finalize();
        }
    }
}

pub(super) struct SelectedTrack {
    pub(super) id: u32,
    pub(super) params: AudioCodecParameters,
    pub(super) time_base: Option<TimeBase>,
    pub(super) start: Timestamp,
    pub(super) duration: Option<Duration>,
    pub(super) delay: u32,
}

pub(super) fn select_track(format: &dyn FormatReader) -> Result<SelectedTrack, DecodeError> {
    let track = format.default_track(TrackType::Audio).ok_or(DecodeError::NoAudioTrack)?;
    let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
        return Err(DecodeError::MissingCodecParameters);
    };
    Ok(SelectedTrack {
        id: track.id,
        params: params.clone(),
        time_base: track.time_base,
        start: track.start_ts,
        duration: track.duration,
        delay: track.delay.unwrap_or(0),
    })
}

pub(super) fn make_decoder(params: &AudioCodecParameters) -> Result<DecoderKind, DecodeError> {
    if params.codec == CODEC_ID_AAC
        && let Some(decoder) = AacDecoder::open(params)
    {
        return Ok(DecoderKind::Aac(Box::new(decoder)));
    }
    if params.codec == CODEC_ID_OPUS {
        let channels = params.channels.as_ref().map_or(0, symphonia::core::audio::Channels::count);
        if !(1..=2).contains(&channels) {
            return Err(DecodeError::UnsupportedOpusChannels { channels });
        }
        let decoder = opus_rs::OpusDecoder::new(SAMPLE_RATE as i32, channels)
            .map_err(|message| DecodeError::OpusInitialize { message })?;
        return Ok(DecoderKind::Opus { decoder: Box::new(decoder), channels });
    }
    symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map(DecoderKind::Media)
        .map_err(|source| DecodeError::UnsupportedCodec { source })
}

pub(super) fn decoder_applies_packet_trims(codec: AudioCodecId) -> bool {
    matches!(codec, CODEC_ID_MP1 | CODEC_ID_MP2 | CODEC_ID_MP3 | CODEC_ID_VORBIS)
}

pub(super) fn format_from_params(params: &AudioCodecParameters) -> Option<AudioFormat> {
    AudioFormat::new(params.sample_rate?, params.channels.as_ref()?.count())
}

pub(super) fn duration_millis(
    time_base: Option<TimeBase>,
    duration: Option<Duration>,
) -> Option<Millis> {
    let millis = time_base?.calc_duration(duration?)?.as_millis();
    u64::try_from(millis).ok().map(Millis::new)
}
