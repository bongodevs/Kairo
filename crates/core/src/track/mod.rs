pub mod codec;

mod builder;
mod info;
mod instance;
mod playlist;
mod source_data;

pub use self::builder::TrackBuilder;
pub use self::info::{TrackInfo, UNKNOWN_AUTHOR, UNKNOWN_TITLE};
pub use self::instance::{Track, TrackSnapshot};
pub use self::playlist::{Playlist, PlaylistError, PlaylistKind};
pub use self::source_data::SourceData;

#[cfg(test)]
#[path = "../../tests/track_codec.rs"]
mod codec_tests;
#[cfg(test)]
#[path = "../../tests/track_model.rs"]
mod model_tests;
#[cfg(test)]
#[path = "../../tests/track_playlist.rs"]
mod playlist_tests;
