use std::sync::Arc;

use player::track::codec::{SourcePayloadCodec, SourceTrackCodec, TrackCodecError};
use player::track::{SourceData, TrackInfo};
use serde_json::{Map, Value};

const MAGIC: &[u8; 5] = b"KPLG\x01";
const NONE: u8 = 0;
const OPAQUE: u8 = 1;

pub(crate) struct PluginCodec {
    source: String,
}

impl PluginCodec {
    pub(crate) fn new(source: impl Into<String>) -> Self {
        Self { source: source.into() }
    }
}

impl SourceTrackCodec for PluginCodec {
    fn source_name(&self) -> &str {
        &self.source
    }
}

impl SourcePayloadCodec for PluginCodec {
    fn encode(
        &self,
        source_data: &SourceData,
        plugin_info: &Map<String, Value>,
    ) -> Result<Vec<u8>, TrackCodecError> {
        let info = serde_json::to_vec(plugin_info).map_err(|_| TrackCodecError::WrongSourceData)?;
        let info_len = u32::try_from(info.len()).map_err(|_| TrackCodecError::FieldTooLong)?;
        let (kind, data): (u8, &[u8]) = match source_data {
            SourceData::None => (NONE, &[]),
            SourceData::Opaque(data) => (OPAQUE, data),
            _ => return Err(TrackCodecError::WrongSourceData),
        };
        let mut payload = Vec::with_capacity(MAGIC.len() + 5 + info.len() + data.len());
        payload.extend_from_slice(MAGIC);
        payload.push(kind);
        payload.extend_from_slice(&info_len.to_be_bytes());
        payload.extend_from_slice(&info);
        payload.extend_from_slice(data);
        Ok(payload)
    }

    fn decode(
        &self,
        payload: &[u8],
        _info: &TrackInfo,
    ) -> Result<(SourceData, Map<String, Value>), TrackCodecError> {
        let Some(payload) = payload.strip_prefix(MAGIC) else {
            return Err(TrackCodecError::WrongSourceData);
        };
        let (&kind, payload) = payload.split_first().ok_or(TrackCodecError::Truncated)?;
        let (length, payload) = payload.split_at_checked(4).ok_or(TrackCodecError::Truncated)?;
        let length =
            u32::from_be_bytes(length.try_into().map_err(|_| TrackCodecError::Truncated)?) as usize;
        let (info, data) = payload.split_at_checked(length).ok_or(TrackCodecError::Truncated)?;
        let plugin_info =
            serde_json::from_slice(info).map_err(|_| TrackCodecError::WrongSourceData)?;
        let source_data = match kind {
            NONE if data.is_empty() => SourceData::None,
            OPAQUE => SourceData::Opaque(Arc::from(data)),
            _ => return Err(TrackCodecError::WrongSourceData),
        };
        Ok((source_data, plugin_info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> TrackInfo {
        TrackInfo {
            identifier: String::from("id"),
            is_seekable: true,
            author: String::from("Artist"),
            length: None,
            is_stream: false,
            title: String::from("Title"),
            uri: None,
            source_name: String::from("demo"),
            artwork_url: None,
            isrc: None,
        }
    }

    #[test]
    fn plugin_info_and_opaque_data_round_trip() {
        let codec = PluginCodec::new("demo");
        let plugin_info = Map::from_iter([
            (String::from("preview"), Value::Bool(true)),
            (String::from("album"), Value::String(String::from("Demo"))),
        ]);
        let source_data = SourceData::Opaque(Arc::from(&b"opaque"[..]));

        let payload = codec.encode(&source_data, &plugin_info).expect("encode");
        let (decoded_data, decoded_info) = codec.decode(&payload, &info()).expect("decode");

        assert_eq!(decoded_data, source_data);
        assert_eq!(decoded_info, plugin_info);
    }

    #[test]
    fn empty_source_data_keeps_plugin_info() {
        let codec = PluginCodec::new("demo");
        let plugin_info = Map::from_iter([(String::from("preview"), Value::Bool(false))]);
        let payload = codec.encode(&SourceData::None, &plugin_info).expect("encode");

        let (source_data, decoded_info) = codec.decode(&payload, &info()).expect("decode");
        assert_eq!(source_data, SourceData::None);
        assert_eq!(decoded_info, plugin_info);
    }

    #[test]
    fn foreign_payloads_are_not_claimed() {
        let codec = PluginCodec::new("demo");
        assert_eq!(
            codec.decode(b"other", &info()).expect_err("foreign payload"),
            TrackCodecError::WrongSourceData
        );
    }
}
