//! Compatibility surface for the original playback module.

pub use crate::ExceptionSeverity;
pub use crate::config::{
    DEFAULT_CLEANUP_THRESHOLD_MS, DEFAULT_STUCK_THRESHOLD_MS, MIN_STUCK_THRESHOLD_MS, PlayerConfig,
};
pub use crate::error::PlayerError;
pub use crate::event::{PlayerEvent, PlayerEvents, TrackEndReason, TrackException};
pub use crate::player::{Player, PlayerGuard, PlayerState, spawn_player};
#[cfg(test)]
pub(crate) mod owner {
    pub(crate) use crate::player::PlayerOwner;
}

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) mod worker {
    pub(crate) use crate::pipeline::cpu;
    pub(crate) use crate::pipeline::{Playback, WorkerEnd, WorkerState};
}

#[cfg(test)]
#[path = "../tests/playback_channel_low_pass.rs"]
mod channel_low_pass_tests;

#[cfg(test)]
#[path = "../tests/playback_control.rs"]
mod control_tests;

#[cfg(test)]
#[path = "../tests/playback_cpu.rs"]
mod cpu_tests;

#[cfg(test)]
#[path = "../tests/playback_crossfade.rs"]
mod crossfade_tests;

#[cfg(test)]
#[path = "../tests/playback_distortion.rs"]
mod distortion_tests;

#[cfg(test)]
#[path = "../tests/playback_echo.rs"]
mod echo_tests;

#[cfg(test)]
#[path = "../tests/playback_events.rs"]
mod events_tests;

#[cfg(test)]
#[path = "../tests/playback_filter_contract.rs"]
mod filter_contract_tests;

#[cfg(test)]
#[path = "../tests/playback_filter_routing.rs"]
mod filter_routing_tests;

#[cfg(test)]
#[path = "../tests/playback_normalization.rs"]
mod normalization_tests;

#[cfg(test)]
#[path = "../tests/playback_pass.rs"]
mod pass_tests;

#[cfg(test)]
#[path = "../tests/playback_rotation.rs"]
mod rotation_tests;

#[cfg(test)]
#[path = "../tests/playback_seek.rs"]
mod seek_tests;

#[cfg(test)]
#[path = "../tests/playback_state.rs"]
mod state_tests;

#[cfg(test)]
#[path = "../tests/playback_volume.rs"]
mod volume_tests;

#[cfg(test)]
#[path = "../tests/playback_scale.rs"]
mod scale_tests;

#[cfg(test)]
#[path = "../tests/playback_support.rs"]
pub(super) mod test_support;
