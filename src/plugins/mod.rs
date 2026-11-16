mod adapter;
pub(crate) mod codec;
mod download;
mod guard;
mod health;
mod host_api;
mod load;
mod manager;
mod mirror;
mod native;
mod placeholder;
mod slot;
mod stage;

pub(crate) use manager::{PluginEntry, PluginManager, ReloadError};
pub(crate) use placeholder::PlaceholderLog;

/// The generated silence stream, exposed for player-level event tests.
#[cfg(test)]
pub(crate) fn generated_silence(
    source: &str,
    reference: &player::source::SourceReference,
    metadata: &player::source::SourceMetadata,
) -> player::source::OpenedSource {
    placeholder::opened_source(source, reference, metadata)
}

#[cfg(test)]
#[path = "../tests/resilience.rs"]
mod resilience_tests;
