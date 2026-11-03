//! Client sessions: the socket a client holds, the players it owns, and their events.

pub mod context;
pub mod lyrics;
pub mod manager;
pub mod player;

pub use context::SocketContext;
pub(crate) use context::OUTBOUND_CAPACITY;
pub use manager::SocketServer;
pub use player::{AudioLossCounter, LavalinkPlayer, VoiceConnectError};
