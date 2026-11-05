//! The Lavalink v4 JSON wire protocol.
//!
//! Every type here serialises to the same JSON a Lavalink v4 node produces, down to field casing
//! (`#[serde(rename_all = "camelCase")]` plus explicit renames where they differ), so existing
//! clients work unchanged.
//!
//! Payloads that patch existing state use [`Omissible`](omissible::Omissible) to tell an absent
//! field, meaning leave unchanged, from a `null` one, meaning clear.

pub mod filters;
pub mod info;
pub mod load_result;
pub mod lyrics;
pub mod message;
pub mod omissible;
pub mod player;
pub mod routeplanner;
pub mod search;
pub mod session;
pub mod sponsorblock;
pub mod stats;
pub mod track;

pub use filters::Filters;
pub use info::{Git, Info, Plugin, Version};
pub use load_result::{Exception, LoadResult, Playlist, PlaylistInfo, Severity};
pub use lyrics::{Line, Lyrics};
pub use message::{EmittedEvent, Message, TrackEndReason};
pub use omissible::Omissible;
pub use player::{
    CrossfadeCurve, CrossfadeSettings, Player, PlayerState, PlayerUpdate, PlayerUpdateTrack,
    VoiceState,
};
pub use search::{SearchResult, SearchText};
pub use session::{Session, SessionUpdate};
pub use stats::{Cpu, FrameStats, Memory, Stats};
pub use track::{Track, TrackInfo};
