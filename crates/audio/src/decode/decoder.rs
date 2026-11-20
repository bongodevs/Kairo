use std::sync::Arc;

use symphonia::core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia::core::errors::Error as MediaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;
use tokio::runtime::Handle;

use kairo_core::{OpenedSource, SourceHints};

use super::codec::{
    DecoderKind, SelectedTrack, decoder_applies_packet_trims, duration_millis, format_from_params,
    make_decoder, select_track,
};
use super::input::DecoderInput;
use super::metadata::{itunsmpb, refresh};
use super::mkv::{self, EndTrim};
use super::mp4;
use super::timeline::{
    SeekState, TrimInput, after_block, decoded_window, millis_from_nanos, packet_nanos,
    packet_position,
};
use super::{
    AudioBlock, AudioFormat, DecodeError, DecodeStats, Millis, OpusBlock, Packet, PcmBlock,
    StreamInfo, TrackMetadata,
};
use crate::SAMPLE_RATE;
use crate::opus::{OpusPacketInfo, OpusRoute, inspect_packet};

/// A demuxer and decoder confined to one blocking worker for its whole life.
pub struct BlockingDecoder {
    format: Box<dyn FormatReader>,
    decoder: DecoderKind,
    selected: SelectedTrack,
    info: StreamInfo,
    samples: Vec<f32>,
    current_packet: Option<Packet>,
    trim_remaining: usize,
    initial_trim: usize,
    end_trim: Option<EndTrim>,
    presentation: Option<Presentation>,
    presentation_remaining: Option<u64>,
    seek: SeekState,
    fallback_nanos: u128,
    stats: DecodeStats,
    finished: bool,
    interrupt: Arc<super::InterruptHandle>,
}

#[derive(Debug, Clone, Copy)]
struct Presentation {
    total: u64,
    sample_rate: u32,
}

impl BlockingDecoder {
    /// Probe an opened source. Call this and [`Self::next_block`] only from a blocking worker.
    pub fn open(source: OpenedSource, runtime: Handle) -> Result<Self, DecodeError> {
        let source_name = source.source_name().to_owned();
        let identifier = source.reference().identifier().to_owned();
        let mut source_metadata = source.metadata().clone();
        let metadata_updates = source.metadata_updates();
        let seekable = source.is_seekable();
        let has_mp4_hint = hints_are_mp4(source.hints());
        let has_mkv_hint = hints_are_matroska(source.hints());
        let mut hint = Hint::new();
        if let Some(extension) = source.hints().file_extension.as_deref() {
            hint.with_extension(extension);
        }
        if let Some(mime_type) = source.hints().mime_type.as_deref() {
            hint.mime_type(mime_type);
        }

        let (mut input, interrupt) = DecoderInput::new(source, runtime);
        let edit_list = (seekable && has_mp4_hint).then(|| mp4::read(&mut input)).flatten();
        let matroska = (seekable && has_mkv_hint).then(|| mkv::read(&mut input)).flatten();
        let stream = MediaSourceStream::new(
            Box::new(input),
            MediaSourceStreamOptions { buffer_len: 64 * 1024 },
        );
        let mut format = symphonia::default::get_probe()
            .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
            .map_err(|source| DecodeError::Probe { source })?;
        let selected = select_track(format.as_ref())?;
        let decoder = make_decoder(&selected.params)?;
        if let Some(updates) = metadata_updates
            && let Ok(updates) = updates.lock()
        {
            source_metadata.title = source_metadata.title.or_else(|| updates.title.clone());
            source_metadata.author = source_metadata.author.or_else(|| updates.author.clone());
            source_metadata.artwork_url =
                source_metadata.artwork_url.or_else(|| updates.artwork_url.clone());
            source_metadata.isrc = source_metadata.isrc.or_else(|| updates.isrc.clone());
        }
        let mut info = StreamInfo {
            source_name,
            identifier,
            codec: decoder.name(),
            container: format.format_info().short_name,
            native_format: format_from_params(&selected.params),
            duration: duration_millis(selected.time_base, selected.duration),
            seekable,
            metadata: TrackMetadata {
                title: source_metadata.title,
                artist: source_metadata.author,
                artwork_url: source_metadata.artwork_url,
                isrc: source_metadata.isrc,
            },
        };
        refresh(format.as_mut(), selected.id, &mut info.metadata, true);
        let gapless = if selected.params.codec == CODEC_ID_AAC {
            edit_list
                .and_then(|edit| selected.params.sample_rate.and_then(|rate| edit.window(rate)))
                .or_else(|| itunsmpb(format.as_mut()))
        } else {
            None
        };
        let presentation = gapless.and_then(|window| {
            let rate = selected.params.sample_rate?;
            window.total.map(|total| Presentation { total, sample_rate: rate })
        });
        let matroska_padding =
            matroska.as_ref().and_then(|tracks| tracks.track(u64::from(selected.id)));
        let matroska_priming = if selected.delay == 0 {
            matroska_padding
                .zip(selected.params.sample_rate)
                .map_or(0, |(padding, rate)| padding.priming_frames(rate))
        } else {
            0
        };
        let end_trim =
            matroska_padding.zip(selected.params.sample_rate).and_then(|(padding, rate)| {
                selected.time_base.and_then(|time_base| padding.end_trim(rate, time_base))
            });
        let priming = u64::from(selected.delay)
            .saturating_add(gapless.map_or(0, |window| window.priming))
            .saturating_add(u64::try_from(matroska_priming).unwrap_or(u64::MAX));
        let initial_trim = usize::try_from(priming).unwrap_or(usize::MAX);

        Ok(Self {
            format,
            decoder,
            selected,
            info,
            samples: Vec::new(),
            current_packet: None,
            trim_remaining: initial_trim,
            initial_trim,
            end_trim,
            presentation,
            presentation_remaining: presentation.map(|presentation| presentation.total),
            seek: SeekState::default(),
            fallback_nanos: 0,
            stats: DecodeStats::default(),
            finished: false,
            interrupt,
        })
    }

    #[must_use]
    pub fn interrupt_handle(&self) -> Arc<super::InterruptHandle> {
        Arc::clone(&self.interrupt)
    }

    #[must_use]
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    #[must_use]
    pub fn stats(&self) -> DecodeStats {
        self.stats
    }

    /// Seek to a millisecond position. Unseekable streams ignore the request.
    pub fn seek(&mut self, position: Millis) -> Result<bool, DecodeError> {
        if !self.info.seekable {
            return Ok(false);
        }
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::from_millis_u64(position.get()),
                    track_id: Some(self.selected.id),
                },
            )
            .map_err(|source| DecodeError::Seek { source })?;
        self.decoder = make_decoder(&self.selected.params)?;
        self.trim_remaining = if position.get() == 0 { self.initial_trim } else { 0 };
        self.presentation_remaining = self.presentation.map(|presentation| {
            if position.get() == 0 || presentation.sample_rate == 0 {
                return presentation.total;
            }
            let played = u128::from(position.get()) * u128::from(presentation.sample_rate) / 1000;
            presentation.total.saturating_sub(u64::try_from(played).unwrap_or(u64::MAX))
        });
        self.seek.reset(position);
        self.fallback_nanos = u128::from(position.get()) * 1_000_000;
        self.current_packet = None;
        self.finished = false;
        Ok(true)
    }

    /// The timeline position a packet will present, without decoding it. Used to schedule a
    /// crossfade at a track position the worker has not encoded yet.
    #[must_use]
    pub fn packet_position(&self, packet: &Packet) -> Millis {
        let nanos = packet_nanos(self.selected.time_base, self.selected.start, packet.pts);
        millis_from_nanos(packet_position(nanos, self.fallback_nanos))
    }

    /// Decode the next usable block, never taking the passthrough route.
    pub fn next_block(&mut self) -> Result<Option<PcmBlock<'_>>, DecodeError> {
        match self.next_audio(OpusRoute::decode())? {
            Some(AudioBlock::Pcm(block)) => Ok(Some(block)),
            Some(AudioBlock::Opus(_)) => Err(DecodeError::UnexpectedPassthrough),
            None => Ok(None),
        }
    }

    /// Produce PCM or a codec-free Opus packet according to the current processing state.
    pub fn next_audio(&mut self, route: OpusRoute) -> Result<Option<AudioBlock<'_>>, DecodeError> {
        let audio = loop {
            let Some(packet) = self.read_packet()? else {
                return Ok(None);
            };
            if let Some(audio) = self.decode_packet(packet, route)? {
                break audio;
            }
        };
        self.audio_block(audio).map(Some)
    }

    fn rebuild_for_container_change(&mut self) -> Result<(), DecodeError> {
        let selected = select_track(self.format.as_ref())?;
        let decoder = make_decoder(&selected.params)?;
        self.info.codec = decoder.name();
        self.info.container = self.format.format_info().short_name;
        self.info.native_format = format_from_params(&selected.params);
        self.info.duration = duration_millis(selected.time_base, selected.duration);
        refresh(self.format.as_mut(), selected.id, &mut self.info.metadata, true);
        self.selected = selected;
        self.decoder = decoder;
        self.trim_remaining = usize::try_from(self.selected.delay).unwrap_or(usize::MAX);
        self.end_trim = None;
        Ok(())
    }

    fn rebuild_for_codec_change(&mut self) -> Result<(), DecodeError> {
        let DecoderKind::Media(current) = &self.decoder else {
            return Ok(());
        };
        let params = current.codec_params().clone();
        let decoder = make_decoder(&params)?;
        self.info.codec = decoder.name();
        self.info.native_format = format_from_params(&params);
        self.selected.params = params;
        self.decoder = decoder;
        Ok(())
    }
}

fn hints_are_mp4(hints: &SourceHints) -> bool {
    const EXTENSIONS: [&str; 5] = ["m4a", "m4b", "m4p", "mp4", "mov"];
    hints
        .file_extension
        .as_deref()
        .is_some_and(|ext| EXTENSIONS.iter().any(|k| ext.eq_ignore_ascii_case(k)))
        || hints.mime_type.as_deref().is_some_and(|m| {
            let m = m.to_ascii_lowercase();
            m.contains("mp4") || m.contains("m4a") || m.contains("quicktime")
        })
}

fn hints_are_matroska(hints: &SourceHints) -> bool {
    const EXTENSIONS: [&str; 5] = ["mkv", "mka", "mks", "mk3d", "webm"];
    hints
        .file_extension
        .as_deref()
        .is_some_and(|ext| EXTENSIONS.iter().any(|k| ext.eq_ignore_ascii_case(k)))
        || hints.mime_type.as_deref().is_some_and(|m| {
            let m = m.to_ascii_lowercase();
            m.contains("matroska") || m.contains("webm")
        })
}

struct DecodedPcm {
    format: AudioFormat,
    start: usize,
    frames: usize,
    position: Millis,
}

impl BlockingDecoder {
    /// The end trim for the final Matroska packet, if this is that packet.
    fn end_trim_for(&self, pts: i64) -> usize {
        self.end_trim.filter(|trim| trim.expected_pts == pts).map_or(0, |trim| trim.frames)
    }

    fn decode_media_packet(
        &mut self,
        packet: &Packet,
        packet_position_nanos: Option<i128>,
    ) -> Result<Option<DecodedPcm>, DecodeError> {
        let (format, decoded_frames, packet_trimmed_by_decoder) = match &mut self.decoder {
            DecoderKind::Media(decoder) => {
                let packet_trimmed_by_decoder =
                    decoder_applies_packet_trims(decoder.codec_params().codec);
                let decoded = match decoder.decode(packet) {
                    Ok(decoded) => decoded,
                    Err(MediaError::IoError(_) | MediaError::DecodeError(_)) => {
                        self.stats.skipped_packets += 1;
                        return Ok(None);
                    }
                    Err(MediaError::ResetRequired) => {
                        self.stats.skipped_packets += 1;
                        self.rebuild_for_codec_change()?;
                        return Ok(None);
                    }
                    Err(source) => return Err(DecodeError::Decode { source }),
                };
                if decoded.is_empty() {
                    return Ok(None);
                }
                let format =
                    AudioFormat::new(decoded.spec().rate(), decoded.spec().channels().count())
                        .ok_or(DecodeError::InvalidAudioFormat)?;
                let decoded_frames = decoded.frames();
                decoded.copy_to_vec_interleaved(&mut self.samples);
                (format, decoded_frames, packet_trimmed_by_decoder)
            }
            DecoderKind::Aac(decoder) => {
                let Some(decoded) = decoder.decode(&packet.data, &mut self.samples)? else {
                    self.stats.skipped_packets += 1;
                    return Ok(None);
                };
                (decoded.format, decoded.frames, false)
            }
            DecoderKind::Opus { .. } => return Err(DecodeError::UnexpectedPassthrough),
        };
        let delay_start = if packet_trimmed_by_decoder {
            self.trim_remaining = 0;
            0
        } else {
            let trim = self.trim_remaining.min(decoded_frames);
            self.trim_remaining -= trim;
            trim
        };
        let packet_end = usize::try_from(packet.trim_end.get())
            .unwrap_or(usize::MAX)
            .saturating_add(self.end_trim_for(packet.pts.get()));
        let window = decoded_window(
            TrimInput {
                decoded_frames,
                sample_rate: format.sample_rate,
                packet_position_nanos,
                fallback_nanos: self.fallback_nanos,
                delay_start,
                packet_start: usize::try_from(packet.trim_start.get()).unwrap_or(usize::MAX),
                packet_end,
                packet_trimmed_by_decoder,
            },
            &mut self.seek,
        );
        let frames = match self.presentation_remaining {
            Some(remaining) => {
                let shown = window.frames.min(usize::try_from(remaining).unwrap_or(usize::MAX));
                self.presentation_remaining = Some(remaining.saturating_sub(shown as u64));
                if self.presentation_remaining == Some(0) {
                    self.finished = true;
                }
                shown
            }
            None => window.frames,
        };
        if frames == 0 {
            return Ok(None);
        }
        let start = window.start * format.channels;
        self.info.native_format = Some(format);
        self.fallback_nanos = after_block(window.position_nanos, frames, format.sample_rate);
        self.stats.decoded_packets += 1;

        Ok(Some(DecodedPcm {
            format,
            start,
            frames,
            position: millis_from_nanos(window.position_nanos),
        }))
    }
}

pub enum PacketAudio {
    Opus { info: OpusPacketInfo, position: Millis },
    Pcm { start: usize, end: usize, format: AudioFormat, frames: usize, position: Millis },
}

impl BlockingDecoder {
    pub fn read_packet(&mut self) -> Result<Option<Packet>, DecodeError> {
        if self.finished {
            return Ok(None);
        }
        self.current_packet = None;

        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.finished = true;
                    self.decoder.finalize();
                    return Ok(None);
                }
                Err(MediaError::ResetRequired) => {
                    self.rebuild_for_container_change()?;
                    continue;
                }
                Err(source) => return Err(DecodeError::Demux { source }),
            };
            refresh(self.format.as_mut(), self.selected.id, &mut self.info.metadata, false);
            if packet.track_id == self.selected.id {
                return Ok(Some(packet));
            }
        }
    }

    pub fn decode_packet(
        &mut self,
        packet: Packet,
        route: OpusRoute,
    ) -> Result<Option<PacketAudio>, DecodeError> {
        let packet_position_nanos =
            packet_nanos(self.selected.time_base, self.selected.start, packet.pts);
        let position_nanos = packet_position(packet_position_nanos, self.fallback_nanos);
        let position = millis_from_nanos(position_nanos);
        if matches!(&self.decoder, DecoderKind::Media(_) | DecoderKind::Aac(_)) {
            let Some(block) = self.decode_media_packet(&packet, packet_position_nanos)? else {
                return Ok(None);
            };
            let end = block.start + block.frames * block.format.channels;
            return Ok(Some(PacketAudio::Pcm {
                start: block.start,
                end,
                format: block.format,
                frames: block.frames,
                position: block.position,
            }));
        }
        let end_trim = self.end_trim_for(packet.pts.get());
        let DecoderKind::Opus { decoder, channels } = &mut self.decoder else {
            return Err(DecodeError::UnexpectedPassthrough);
        };
        let info = match inspect_packet(&packet.data) {
            Ok(info) => info,
            Err(_) => {
                self.stats.skipped_packets += 1;
                return Ok(None);
            }
        };
        let packet_trim_start = usize::try_from(packet.trim_start.get()).unwrap_or(usize::MAX);
        let packet_trim_end = usize::try_from(packet.trim_end.get()).unwrap_or(usize::MAX);
        if route.allows_passthrough()
            && *channels == 2
            && info.can_passthrough_voice(packet.data.len())
            && self.trim_remaining == 0
            && packet_trim_start == 0
            && packet_trim_end == 0
            && end_trim == 0
            && self.seek.allows_passthrough(position_nanos)
        {
            self.fallback_nanos = after_block(position_nanos, info.total_samples, SAMPLE_RATE);
            self.stats.passthrough_packets += 1;
            self.current_packet = Some(packet);
            return Ok(Some(PacketAudio::Opus { info, position }));
        }

        let samples = info.total_samples.saturating_mul(*channels);
        self.samples.resize(samples, 0.0);
        let decoded_frames =
            match decoder.decode(&packet.data, info.total_samples, &mut self.samples) {
                Ok(frames) => frames,
                Err(_) => {
                    self.stats.skipped_packets += 1;
                    return Ok(None);
                }
            };
        let delay_trim = self.trim_remaining.min(decoded_frames);
        self.trim_remaining -= delay_trim;
        let window = decoded_window(
            TrimInput {
                decoded_frames,
                sample_rate: SAMPLE_RATE,
                packet_position_nanos,
                fallback_nanos: self.fallback_nanos,
                delay_start: delay_trim,
                packet_start: packet_trim_start,
                packet_end: packet_trim_end.saturating_add(end_trim),
                packet_trimmed_by_decoder: false,
            },
            &mut self.seek,
        );
        if window.frames == 0 {
            return Ok(None);
        }
        let start = window.start * *channels;
        let end = start + window.frames * *channels;
        let format =
            AudioFormat::new(SAMPLE_RATE, *channels).ok_or(DecodeError::InvalidAudioFormat)?;
        self.info.native_format = Some(format);
        self.fallback_nanos = after_block(window.position_nanos, window.frames, SAMPLE_RATE);
        self.stats.decoded_packets += 1;
        Ok(Some(PacketAudio::Pcm {
            start,
            end,
            format,
            frames: window.frames,
            position: millis_from_nanos(window.position_nanos),
        }))
    }

    pub fn audio_block(&self, audio: PacketAudio) -> Result<AudioBlock<'_>, DecodeError> {
        match audio {
            PacketAudio::Opus { info, position } => {
                let packet =
                    self.current_packet.as_ref().ok_or(DecodeError::UnexpectedPassthrough)?;
                Ok(AudioBlock::Opus(OpusBlock { packet: &packet.data, info, position }))
            }
            PacketAudio::Pcm { start, end, format, frames, position } => {
                Ok(AudioBlock::Pcm(PcmBlock {
                    samples: &self.samples[start..end],
                    format,
                    frames,
                    position,
                }))
            }
        }
    }
}
