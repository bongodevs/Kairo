pub(crate) mod client;
pub(crate) mod handshake;
pub(crate) mod heartbeat;
pub(crate) mod pump;
pub(crate) mod socket;
pub(crate) mod wire;

pub(crate) use client::{CloseAction, Gateway, Ready, close_action, reconnect_delay};
pub use client::{GatewayError, GatewaySocket, VoiceTarget};
pub(crate) use handshake::{Intro, Opening};
pub use heartbeat::Ping;
pub(crate) use heartbeat::{Heartbeat, acked};
pub(crate) use pump::{Handled, Pump, heartbeat_loop_with_pending};
pub use socket::{Inbound, Socket};
pub use wire::VoiceEvent;
pub(crate) use wire::{Reply, binary_reply, frame_text, json_reply};
pub const DAVE_PROTOCOL_VERSION: u16 = 1;

#[cfg(test)]
#[path = "../../tests/protocol.rs"]
mod tests;
