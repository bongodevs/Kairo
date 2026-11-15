mod adapter;
pub(crate) mod codec;
mod download;
mod host_api;
mod manager;
mod mirror;
mod native;
mod stage;

pub(crate) use manager::PluginManager;
