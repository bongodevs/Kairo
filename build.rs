use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let commit = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let commit_time_ms = git(&["log", "-1", "--format=%ct"])
        .and_then(|s| s.parse::<i64>().ok())
        .map(|secs| secs * 1000)
        .unwrap_or(0);
    let built = SystemTime::now().duration_since(UNIX_EPOCH).ok();
    let build_time_ms =
        built.and_then(|duration| i64::try_from(duration.as_millis()).ok()).unwrap_or(-1);
    let build_time =
        built.map_or_else(|| "unknown".into(), |duration| utc_timestamp(duration.as_secs()));
    let player_version = locked_version("player").unwrap_or_else(|| "unknown".into());
    let voice_version = locked_version("voice").unwrap_or_else(|| "unknown".into());
    let rustc_version = command("rustc", &["--version"]).unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=KAIRO_GIT_BRANCH={branch}");
    println!("cargo:rustc-env=KAIRO_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=KAIRO_GIT_COMMIT_TIME_UNIX_MS={commit_time_ms}");
    println!("cargo:rustc-env=KAIRO_BUILD_TIME={build_time}");
    println!("cargo:rustc-env=KAIRO_BUILD_TIME_UNIX_MS={build_time_ms}");
    println!("cargo:rustc-env=KAIRO_PLAYER_VERSION={player_version}");
    println!("cargo:rustc-env=KAIRO_VOICE_VERSION={voice_version}");
    println!("cargo:rustc-env=KAIRO_RUSTC_VERSION={rustc_version}");

    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=Cargo.lock");
}

fn utc_timestamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (h, m, s) = (rem / 3_600, rem % 3_600 / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02} +00:00")
}

fn locked_version(package: &str) -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let lock = std::fs::read_to_string(format!("{manifest_dir}/Cargo.lock")).ok()?;
    let mut in_target = false;
    for line in lock.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("name = \"").and_then(|s| s.strip_suffix('"')) {
            in_target = name == package;
        } else if in_target
            && let Some(version) =
                line.strip_prefix("version = \"").and_then(|s| s.strip_suffix('"'))
        {
            return Some(version.to_string());
        }
    }
    None
}

fn git(args: &[&str]) -> Option<String> {
    command("git", args)
}

fn command(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}
