//! Startup odds and ends: logging, the console banner, and the update check.

pub(crate) mod ansi;
pub mod banner;
pub mod lock;
mod logging;
pub mod time;
pub mod update;

pub use lock::locked;
pub use logging::init;
