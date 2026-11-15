use axum::Json;
use axum::extract::State;
use serde::Serialize;

use player::source::Sources;

use crate::app::AppState;
use crate::players::FilterPolicy;
use crate::plugins::PluginManager;
const VERSION: &str = env!("CARGO_PKG_VERSION");
const JVM: &str = "n/a";
const PLAYER: &str = "n/a";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionInfo {
    semver: String,
    major: u32,
    minor: u32,
    patch: u32,
    pre_release: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Git {
    branch: String,
    commit: String,
    commit_time: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Plugin {
    name: String,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Info {
    version: VersionInfo,
    build_time: i64,
    git: Git,
    jvm: String,
    lavaplayer: String,
    source_managers: Vec<String>,
    filters: Vec<String>,
    plugins: Vec<Plugin>,
}

fn parse_semver(input: &str) -> VersionInfo {
    let (core, pre) = input.split_once('-').unwrap_or((input, ""));
    let mut parts = core.split('.');
    let number = |part: Option<&str>| part.and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    VersionInfo {
        semver: input.to_string(),
        major: number(parts.next()),
        minor: number(parts.next()),
        patch: number(parts.next()),
        pre_release: if pre.is_empty() { None } else { Some(pre.to_string()) },
    }
}

fn epoch_ms(key: Option<&'static str>) -> i64 {
    key.and_then(|value| value.parse::<i64>().ok()).unwrap_or(0)
}

fn build_info(sources: &Sources, filter_policy: FilterPolicy, plugins: &PluginManager) -> Info {
    Info {
        version: parse_semver(VERSION),
        build_time: epoch_ms(option_env!("KAIRO_BUILD_TIME_UNIX_MS")),
        git: Git {
            branch: option_env!("KAIRO_GIT_BRANCH").unwrap_or_default().to_string(),
            commit: option_env!("KAIRO_GIT_COMMIT").unwrap_or_default().to_string(),
            commit_time: epoch_ms(option_env!("KAIRO_GIT_COMMIT_TIME_UNIX_MS")),
        },
        jvm: JVM.to_string(),
        lavaplayer: PLAYER.to_string(),
        source_managers: sources.names().map(str::to_owned).collect(),
        filters: filter_policy.enabled_names().map(str::to_owned).collect(),
        plugins: plugins
            .versions()
            .into_iter()
            .map(|(name, version)| Plugin { name, version })
            .collect(),
    }
}

pub(crate) async fn version() -> &'static str {
    VERSION
}

pub(crate) async fn info(State(state): State<AppState>) -> Json<Info> {
    Json(build_info(state.sources(), state.filter_policy(), state.plugins()))
}

#[cfg(test)]
#[path = "../../tests/info.rs"]
mod tests;
