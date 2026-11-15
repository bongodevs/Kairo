pub mod config;
pub mod connection;
// The hot path the frame benchmark measures; nothing else outside the crate reads them.
pub mod crypto;
pub mod sender;
pub mod udp;

mod dave;
mod gateway;

#[cfg(test)]
#[path = "../tests/scale.rs"]
mod scale;

pub use kairo_core::ids;

pub use config::{CipherPolicy, VoiceConfig};
pub use connection::{ConnectionError, Disconnect, Phase};
pub use connection::{Frames, Lifecycle, Speaking};
pub use connection::{SPEAKING_MICROPHONE, SPEAKING_PRIORITY, SPEAKING_SOUNDSHARE};
pub use gateway::{GatewayError, GatewaySocket, Inbound, Ping, Socket, VoiceEvent, VoiceTarget};
pub use sender::{FrameSource, FrameStats};

// Old module names kept so existing callers keep compiling.
pub use connection as connect;
pub use sender as media;
pub use udp::rtp;
