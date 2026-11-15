use serde_json::{Map, Value};

use kairo_core::track::codec::{
    SourcePayloadCodec, SourcePayloadReader, SourcePayloadWriter, TrackCodecError,
};
use kairo_core::track::{SourceData, TrackInfo};

#[derive(Debug, Clone, Copy, Default)]
pub struct ExtendedTrackCodec;

impl SourcePayloadCodec for ExtendedTrackCodec {
    fn encode(
        &self,
        data: &SourceData,
        plugin_info: &Map<String, Value>,
    ) -> Result<Vec<u8>, TrackCodecError> {
        if !matches!(data, SourceData::None) {
            return Err(TrackCodecError::WrongSourceData);
        }
        let extras = TrackExtras::from_map(plugin_info)?;
        let mut output = SourcePayloadWriter::new();
        output.optional_text(extras.album_name.as_deref())?;
        output.optional_text(extras.album_url.as_deref())?;
        output.optional_text(extras.artist_url.as_deref())?;
        output.optional_text(extras.artist_artwork_url.as_deref())?;
        output.optional_text(extras.preview_url.as_deref())?;
        output.boolean(extras.is_preview);
        Ok(output.finish())
    }

    fn decode(
        &self,
        payload: &[u8],
        info: &TrackInfo,
    ) -> Result<(SourceData, Map<String, Value>), TrackCodecError> {
        if payload.is_empty() {
            return Ok((SourceData::None, TrackExtras::default().into_map(info.uri.clone())));
        }
        let mut input = SourcePayloadReader::new(payload);
        let extras = TrackExtras {
            album_name: input.optional_text()?,
            album_url: input.optional_text()?,
            artist_url: input.optional_text()?,
            artist_artwork_url: input.optional_text()?,
            preview_url: input.optional_text()?,
            is_preview: input.boolean()?,
        };
        // Reject a payload that only partially matches so it stays opaque.
        if input.consumed() != payload.len() {
            return Err(TrackCodecError::WrongSourceData);
        }
        Ok((SourceData::None, extras.into_map(info.uri.clone())))
    }
}

/// Portable metadata fields carried by the extended source payload.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TrackExtras {
    pub(crate) album_name: Option<String>,
    pub(crate) album_url: Option<String>,
    pub(crate) artist_url: Option<String>,
    pub(crate) artist_artwork_url: Option<String>,
    pub(crate) preview_url: Option<String>,
    pub(crate) is_preview: bool,
}

impl TrackExtras {
    pub(crate) fn from_map(info: &Map<String, Value>) -> Result<Self, TrackCodecError> {
        Ok(Self {
            album_name: optional_text(info, "albumName")?,
            album_url: optional_text(info, "albumUrl")?,
            artist_url: optional_text(info, "artistUrl")?,
            artist_artwork_url: optional_text(info, "artistArtworkUrl")?,
            preview_url: optional_text(info, "previewUrl")?,
            is_preview: boolean(info, "isPreview")?,
        })
    }

    pub(crate) fn into_map(self, save_uri: Option<String>) -> Map<String, Value> {
        let mut info = Map::new();
        insert_optional(&mut info, "save_uri", save_uri);
        insert_optional(&mut info, "albumUrl", self.album_url);
        insert_optional(&mut info, "albumName", self.album_name);
        insert_optional(&mut info, "previewUrl", self.preview_url);
        info.insert(String::from("isPreview"), Value::Bool(self.is_preview));
        insert_optional(&mut info, "artistUrl", self.artist_url);
        insert_optional(&mut info, "artistArtworkUrl", self.artist_artwork_url);
        info.insert(String::from("isLocal"), Value::Bool(false));
        info
    }
}

pub(crate) fn insert_optional(info: &mut Map<String, Value>, name: &str, value: Option<String>) {
    info.insert(name.to_owned(), value.map_or(Value::Null, Value::String));
}

fn optional_text(info: &Map<String, Value>, name: &str) -> Result<Option<String>, TrackCodecError> {
    match info.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(TrackCodecError::WrongSourceData),
    }
}

fn boolean(info: &Map<String, Value>, name: &str) -> Result<bool, TrackCodecError> {
    match info.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(TrackCodecError::WrongSourceData),
    }
}
