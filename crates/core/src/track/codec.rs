mod source_text;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};
use thiserror::Error;

use self::source_text::{self as base64, Reader, text};
pub use self::source_text::{SourcePayloadReader, SourcePayloadWriter};
#[cfg(test)]
pub(crate) use self::source_text::{
    decode_base64_for_test, encode_base64_for_test, write_text_for_test,
};
use crate::track::{SourceData, Track, TrackBuilder, TrackInfo, TrackSnapshot};
use crate::{Millis, SourceReference};
const TRACK_INFO_VERSION: u8 = 3;
const VERSIONED_FLAG: u32 = 1;

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrackCodecError {
    #[error("track string is not valid base64")]
    InvalidBase64,
    #[error("encoded track is truncated")]
    Truncated,
    #[error("track text is not valid modified UTF-8")]
    InvalidText,
    #[error("a track field exceeds the 65535-byte modified-UTF-8 limit")]
    FieldTooLong,
    #[error("track has not been resolved")]
    UnresolvedTrack,
    #[error("no source codec is registered for {name:?}")]
    UnknownSource { name: String },
    #[error("source payload does not match its codec")]
    WrongSourceData,
    #[error("a source codec named {name:?} is already registered")]
    DuplicateSource { name: String },
}

#[derive(Debug)]
#[non_exhaustive]
pub enum DecodeTrack {
    Track(Track),
    UnsupportedVersion { version: u8 },
}

pub fn encode(track: &TrackSnapshot, codecs: &TrackCodecs) -> Result<String, TrackCodecError> {
    let info = track.info().ok_or(TrackCodecError::UnresolvedTrack)?;
    let source_data = match codecs.get(&info.source_name) {
        Some(codec) => codec.encode(track.source_data(), track.plugin_info())?,
        None => match (track.source_data(), codecs.fallback()) {
            (SourceData::Opaque(bytes), _) => bytes.to_vec(),
            (_, Some(codec)) => codec.encode(track.source_data(), track.plugin_info())?,
            (_, None) => {
                return Err(TrackCodecError::UnknownSource { name: info.source_name.clone() });
            }
        },
    };
    let mut body = Vec::new();
    body.push(TRACK_INFO_VERSION);
    text::write(&mut body, &info.title)?;
    text::write(&mut body, &info.author)?;
    write_long(&mut body, encoded_length(info.length));
    text::write(&mut body, &info.identifier)?;
    body.push(u8::from(info.is_stream));
    write_optional(&mut body, info.uri.as_deref())?;
    write_optional(&mut body, info.artwork_url.as_deref())?;
    write_optional(&mut body, info.isrc.as_deref())?;
    text::write(&mut body, &info.source_name)?;
    body.extend_from_slice(&source_data);
    write_long(&mut body, i64::try_from(track.encoded_position().get()).unwrap_or(i64::MAX));

    let length = u32::try_from(body.len()).map_err(|_| TrackCodecError::FieldTooLong)?;
    if length > 0x3fff_ffff {
        return Err(TrackCodecError::FieldTooLong);
    }
    let header = length | (VERSIONED_FLAG << 30);
    let mut output = Vec::with_capacity(body.len() + 4);
    output.extend_from_slice(&header.to_be_bytes());
    output.extend_from_slice(&body);
    Ok(base64::encode(&output))
}

pub fn decode(encoded: &str, codecs: &TrackCodecs) -> Result<DecodeTrack, TrackCodecError> {
    let bytes = base64::decode(encoded)?;
    let mut input = Reader::new(&bytes);
    let header = input.int()?;
    let flags = header as u32 >> 30;
    let length = (header & 0x3fff_ffff) as usize;
    input.limit_message(length)?;
    let version = if flags & VERSIONED_FLAG != 0 { input.byte()? } else { 1 };
    if version > TRACK_INFO_VERSION {
        return Ok(DecodeTrack::UnsupportedVersion { version });
    }

    let title = input.text()?;
    let author = input.text()?;
    let length = decoded_length(input.long()?);
    let identifier = input.text()?;
    let is_stream = input.byte()? != 0;
    let uri = if version >= 2 { input.optional_text()? } else { None };
    let artwork_url = if version >= 3 { input.optional_text()? } else { None };
    let isrc = if version >= 3 { input.optional_text()? } else { None };
    let source_name = input.text()?;
    let info = TrackInfo {
        identifier: identifier.clone(),
        is_seekable: !is_stream,
        author,
        length,
        is_stream,
        title: title.clone(),
        uri,
        source_name: source_name.clone(),
        artwork_url,
        isrc,
    };
    let reference = SourceReference::with_title(identifier, title);
    let (source_payload, stored_position) = split_position(input.remaining())?;
    let decoded = match codecs.get(&source_name) {
        Some(codec) => Some(codec.decode(source_payload, &info)?),
        None if source_payload.is_empty() => None,
        None => codecs.fallback().and_then(|codec| codec.decode(source_payload, &info).ok()),
    };
    let (source_data, plugin_info) =
        decoded.unwrap_or_else(|| (SourceData::Opaque(source_payload.into()), Map::new()));
    Ok(DecodeTrack::Track(
        TrackBuilder::new(reference, info)
            .source_data(source_data)
            .plugin_info(plugin_info)
            .initial_position(position(stored_position))
            .build(),
    ))
}

fn split_position(payload: &[u8]) -> Result<(&[u8], i64), TrackCodecError> {
    let boundary = payload.len().checked_sub(size_of::<i64>()).ok_or(TrackCodecError::Truncated)?;
    let (payload, stored) = payload.split_at(boundary);
    Ok((payload, i64::from_be_bytes(stored.try_into().map_err(|_| TrackCodecError::Truncated)?)))
}

fn position(stored: i64) -> Millis {
    Millis::new(u64::try_from(stored.max(0)).unwrap_or(u64::MAX))
}

fn encoded_length(length: Option<Millis>) -> i64 {
    length.map_or(i64::MAX, |length| i64::try_from(length.get()).unwrap_or(i64::MAX))
}

fn decoded_length(length: i64) -> Option<Millis> {
    (length >= 0 && length != i64::MAX).then(|| Millis::new(length as u64))
}

fn write_long(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn write_optional(output: &mut Vec<u8>, value: Option<&str>) -> Result<(), TrackCodecError> {
    if let Some(value) = value {
        output.push(1);
        text::write(output, value)
    } else {
        output.push(0);
        Ok(())
    }
}

/// Encodes and decodes one source payload.
pub trait SourcePayloadCodec: Send + Sync {
    fn encode(
        &self,
        data: &SourceData,
        plugin_info: &Map<String, Value>,
    ) -> Result<Vec<u8>, TrackCodecError>;

    fn decode(
        &self,
        payload: &[u8],
        info: &TrackInfo,
    ) -> Result<(SourceData, Map<String, Value>), TrackCodecError>;
}

/// A payload codec bound to one `sourceName`.
pub trait SourceTrackCodec: SourcePayloadCodec {
    fn source_name(&self) -> &str;
}

pub struct TrackCodecs {
    entries: BTreeMap<String, Box<dyn SourceTrackCodec>>,
    fallback: Option<Box<dyn SourcePayloadCodec>>,
}

impl TrackCodecs {
    #[must_use]
    pub fn new() -> Self {
        let mut entries: BTreeMap<String, Box<dyn SourceTrackCodec>> = BTreeMap::new();
        entries.insert(String::from("kairo"), Box::new(EmptySourceCodec::new("kairo")));
        entries.insert(String::from("http"), Box::new(ProbeCodec("http")));
        entries.insert(String::from("local"), Box::new(ProbeCodec("local")));
        Self { entries, fallback: None }
    }

    pub fn register(
        &mut self,
        codec: impl SourceTrackCodec + 'static,
    ) -> Result<(), TrackCodecError> {
        let name = codec.source_name().to_owned();
        if self.entries.contains_key(&name) {
            return Err(TrackCodecError::DuplicateSource { name });
        }
        self.entries.insert(name, Box::new(codec));
        Ok(())
    }

    /// Use `codec` for source names that have no registered codec.
    pub fn set_fallback(&mut self, codec: impl SourcePayloadCodec + 'static) {
        self.fallback = Some(Box::new(codec));
    }

    pub(super) fn get(&self, name: &str) -> Option<&dyn SourceTrackCodec> {
        self.entries.get(name).map(Box::as_ref)
    }

    pub(super) fn fallback(&self) -> Option<&dyn SourcePayloadCodec> {
        self.fallback.as_deref()
    }
}

impl Default for TrackCodecs {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EmptySourceCodec {
    name: String,
}

impl EmptySourceCodec {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl SourceTrackCodec for EmptySourceCodec {
    fn source_name(&self) -> &str {
        &self.name
    }
}

impl SourcePayloadCodec for EmptySourceCodec {
    fn encode(
        &self,
        data: &SourceData,
        _plugin_info: &Map<String, Value>,
    ) -> Result<Vec<u8>, TrackCodecError> {
        match data {
            SourceData::None => Ok(Vec::new()),
            SourceData::Opaque(bytes) if bytes.is_empty() => Ok(Vec::new()),
            _ => Err(TrackCodecError::WrongSourceData),
        }
    }

    fn decode(
        &self,
        _payload: &[u8],
        _info: &TrackInfo,
    ) -> Result<(SourceData, Map<String, Value>), TrackCodecError> {
        Ok((SourceData::None, Map::new()))
    }
}

struct ProbeCodec(&'static str);

impl SourceTrackCodec for ProbeCodec {
    fn source_name(&self) -> &str {
        self.0
    }
}

impl SourcePayloadCodec for ProbeCodec {
    fn encode(
        &self,
        data: &SourceData,
        _plugin_info: &Map<String, Value>,
    ) -> Result<Vec<u8>, TrackCodecError> {
        let SourceData::Probe { name, parameters } = data else {
            return Err(TrackCodecError::WrongSourceData);
        };
        let mut output = Vec::new();
        if let Some(parameters) = parameters {
            text::write(&mut output, &format!("{name}|{parameters}"))?;
        } else {
            text::write(&mut output, name)?;
        }
        Ok(output)
    }

    fn decode(
        &self,
        payload: &[u8],
        _info: &TrackInfo,
    ) -> Result<(SourceData, Map<String, Value>), TrackCodecError> {
        let (descriptor, _) = text::read(payload)?;
        let (name, parameters) = match descriptor.split_once('|') {
            Some((name, parameters)) => (name.to_owned(), Some(parameters.to_owned())),
            None => (descriptor, None),
        };
        Ok((SourceData::Probe { name, parameters }, Map::new()))
    }
}

pub struct OpaqueSourceCodec {
    name: String,
    payload_length: usize,
}

impl OpaqueSourceCodec {
    #[must_use]
    pub fn new(name: impl Into<String>, payload_length: usize) -> Self {
        Self { name: name.into(), payload_length }
    }
}

impl SourceTrackCodec for OpaqueSourceCodec {
    fn source_name(&self) -> &str {
        &self.name
    }
}

impl SourcePayloadCodec for OpaqueSourceCodec {
    fn encode(
        &self,
        data: &SourceData,
        _plugin_info: &Map<String, Value>,
    ) -> Result<Vec<u8>, TrackCodecError> {
        let SourceData::Opaque(bytes) = data else {
            return Err(TrackCodecError::WrongSourceData);
        };
        if bytes.len() != self.payload_length {
            return Err(TrackCodecError::WrongSourceData);
        }
        Ok(bytes.to_vec())
    }

    fn decode(
        &self,
        payload: &[u8],
        _info: &TrackInfo,
    ) -> Result<(SourceData, Map<String, Value>), TrackCodecError> {
        let payload = payload.get(..self.payload_length).ok_or(TrackCodecError::Truncated)?;
        Ok((SourceData::Opaque(Arc::from(payload)), Map::new()))
    }
}
