mod decode;
mod dto;
mod load;

use player::track::Track;

pub(crate) use decode::{decodetrack, decodetracks};
pub(crate) use dto::{TrackResponse, encode_snapshot};
pub(crate) use load::loadtracks;

/// The advertised source name of a track, or `""` when it carries no metadata.
pub(crate) fn source_name(track: &Track) -> &str {
    track.info().map_or("", |info| info.source_name.as_str())
}
