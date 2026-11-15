pub mod config;
pub mod telemetry;

mod app;
mod error;
mod players;
mod plugins;
mod rest;
mod route_planner;
mod session;
mod stats;
mod tracks;
mod utils;
mod ws;

pub use app::serve;

#[cfg(test)]
#[path = "tests/jiosaavn_live.rs"]
mod jiosaavn_live;
#[cfg(test)]
#[path = "tests/rest.rs"]
mod rest_tests;
#[cfg(test)]
#[path = "tests/support.rs"]
mod test_support;
#[cfg(test)]
#[path = "tests/voice_connection.rs"]
mod voice_connection;
#[cfg(test)]
#[path = "tests/voice_live.rs"]
mod voice_live;
