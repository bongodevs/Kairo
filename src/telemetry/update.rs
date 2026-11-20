use semver::Version;
use serde::Deserialize;

use crate::config::{LogFormat, LoggingConfig};
use crate::telemetry::ansi::{ACCENT, BOLD, DIM, RESET};
const RELEASES_API: &str = "https://api.github.com/repos/bongodevs/Kairo/releases/latest";
const IMAGE: &str = "ghcr.io/bongodevs/kairo:latest";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

pub async fn check(logging: &LoggingConfig, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    if std::env::var_os("KAIRO_NO_UPDATE_CHECK").is_some() || *shutdown.borrow() {
        return;
    }

    let Ok(current) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        return;
    };

    tokio::select! {
        _ = shutdown.wait_for(|&seen| seen) => {}
        release = latest() => {
            let release = match release {
                Ok(Some(release)) => release,
                Ok(None) => return,
                Err(err) => {
                    tracing::debug!("update check failed: {err}");
                    return;
                }
            };

            match upgrade_to(&current, &release.tag_name) {
                Some(latest) => announce(logging, &current, &latest),
                None => tracing::debug!(version = %current, "Kairo is up to date"),
            }
        }
    }
}

fn announce(logging: &LoggingConfig, current: &Version, latest: &Version) {
    if logging.format == LogFormat::Json {
        tracing::warn!(%current, %latest, "a new Kairo release is available");
        return;
    }

    let (accent, bold, dim, reset) =
        if logging.color { (ACCENT, BOLD, DIM, RESET) } else { ("", "", "", "") };

    println!(
        "\n  {accent}▍{reset} {bold}Kairo v{latest} is available{reset}  {dim}· running v{current}{reset}\
         \n  {accent}▍{reset} {dim}docker pull {IMAGE}  ·  then restart the container{reset}\n"
    );
}

fn upgrade_to(current: &Version, tag: &str) -> Option<Version> {
    Version::parse(tag.trim_start_matches('v')).ok().filter(|latest| latest > current)
}

async fn latest() -> reqwest::Result<Option<Release>> {
    let client = reqwest::Client::builder()
        .user_agent(kairo_core::http::USER_AGENT)
        .timeout(kairo_core::http::REQUEST_TIMEOUT)
        .build()?;

    let response =
        client.get(RELEASES_API).header("Accept", "application/vnd.github+json").send().await?;

    if !response.status().is_success() {
        return Ok(None);
    }

    Ok(Some(response.json::<Release>().await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_a_newer_tag() {
        let current = Version::parse("4.6.0").unwrap();
        assert_eq!(upgrade_to(&current, "v4.7.0"), Some(Version::parse("4.7.0").unwrap()));
        assert_eq!(upgrade_to(&current, "5.0.0"), Some(Version::parse("5.0.0").unwrap()));
    }

    #[test]
    fn ignores_same_or_older_tags() {
        let current = Version::parse("4.6.0").unwrap();
        assert_eq!(upgrade_to(&current, "v4.6.0"), None);
        assert_eq!(upgrade_to(&current, "4.5.9"), None);
    }

    #[test]
    fn ignores_unparsable_tags() {
        let current = Version::parse("4.6.0").unwrap();
        assert_eq!(upgrade_to(&current, "nightly"), None);
        assert_eq!(upgrade_to(&current, ""), None);
    }

    #[tokio::test]
    async fn update_check_cancels_on_shutdown() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let logging = LoggingConfig::default();
        let handle = tokio::spawn(async move {
            check(&logging, rx).await;
        });
        tx.send(true).expect("send shutdown");
        tokio::time::timeout(std::time::Duration::from_millis(500), handle)
            .await
            .expect("update check should terminate quickly after shutdown signal")
            .expect("task join");
    }

    #[tokio::test]
    async fn update_check_aborts_immediately_if_already_shutdown() {
        let (_tx, rx) = tokio::sync::watch::channel(true);
        let logging = LoggingConfig::default();
        tokio::time::timeout(std::time::Duration::from_millis(100), check(&logging, rx))
            .await
            .expect("should exit immediately when shutdown is already true");
    }
}
