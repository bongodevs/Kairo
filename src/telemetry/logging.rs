use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt};

use crate::config::{LogFileConfig, LogFormat, LoggingConfig};
use crate::stats::CountingLayer;

const TIME_FORMAT: &str = "%H:%M:%S";
const LOG_FILE_NAME: &str = "kairo.log";

pub fn init(cfg: &LoggingConfig) -> Option<WorkerGuard> {
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();

    layers.push(Box::new(CountingLayer));
    layers.push(fmt_layer(cfg, cfg.color, std::io::stdout));

    let guard = cfg.file.as_ref().and_then(|file| match make_writer(file) {
        Ok(writer) => {
            let (writer, guard) = tracing_appender::non_blocking(writer);
            layers.push(fmt_layer(cfg, false, writer));
            Some(guard)
        }
        Err(error) => {
            eprintln!("could not open log file in {}: {error}", file.path);
            None
        }
    });

    tracing_subscriber::registry().with(layers).with(build_filter(cfg)).init();

    tracing::debug!(
        level = %cfg.level,
        target_overrides = cfg.levels.len(),
        file = cfg.file.is_some(),
        "logging initialized"
    );

    guard
}

fn build_filter(cfg: &LoggingConfig) -> EnvFilter {
    if let Ok(env) = std::env::var("RUST_LOG")
        && !env.trim().is_empty()
    {
        return EnvFilter::builder().parse_lossy(env);
    }
    build_filter_from(cfg)
}

fn build_filter_from(cfg: &LoggingConfig) -> EnvFilter {
    let verbose = is_verbose(&cfg.level);
    let mut directives = String::from(if verbose { "info" } else { cfg.level.as_str() });

    if verbose {
        for target in APP_TARGETS {
            if !is_covered(cfg, target) {
                push(&mut directives, target, &cfg.level);
            }
        }
    }
    for (target, level) in &cfg.levels {
        push(&mut directives, target, level);
    }
    for (target, level) in DEFAULT_SUPPRESSIONS {
        if !overlaps(cfg, target) {
            push(&mut directives, target, level);
        }
    }
    EnvFilter::builder().parse_lossy(directives)
}

fn push(directives: &mut String, target: &str, level: &str) {
    directives.push(',');
    directives.push_str(target);
    directives.push('=');
    directives.push_str(level);
}

fn is_verbose(level: &str) -> bool {
    level.eq_ignore_ascii_case("trace") || level.eq_ignore_ascii_case("debug")
}

/// Whether a `[logging.levels]` entry already applies to `target` or one of its parents.
fn is_covered(cfg: &LoggingConfig, target: &str) -> bool {
    cfg.levels.keys().any(|key| target.starts_with(key.as_str()))
}

/// Whether any `[logging.levels]` entry touches `target`, including one of its children, so a
/// default suppression would duplicate or fight it.
fn overlaps(cfg: &LoggingConfig, target: &str) -> bool {
    cfg.levels.keys().any(|key| target.starts_with(key.as_str()) || key.starts_with(target))
}

/// Workspace targets that follow the configured level at `debug` and `trace`.
const APP_TARGETS: &[&str] = &["kairo", "kairo_core", "kairo_audio", "player", "sources", "voice"];

const DEFAULT_SUPPRESSIONS: &[(&str, &str)] =
    &[("symphonia_format_isomp4::demuxer", "error"), ("symphonia_core::formats::probe", "error")];

fn fmt_layer<W>(
    cfg: &LoggingConfig,
    ansi: bool,
    writer: W,
) -> Box<dyn Layer<Registry> + Send + Sync>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let timer = ChronoLocal::new(TIME_FORMAT.to_owned());
    let base = fmt::layer().with_writer(writer).with_ansi(ansi).with_target(cfg.show_target);

    match cfg.format {
        LogFormat::Compact => {
            let l = base.compact();
            if cfg.timestamps { l.with_timer(timer).boxed() } else { l.without_time().boxed() }
        }
        LogFormat::Pretty => {
            let l = base.pretty();
            if cfg.timestamps { l.with_timer(timer).boxed() } else { l.without_time().boxed() }
        }
        LogFormat::Json => {
            let l = base.json();
            if cfg.timestamps { l.with_timer(timer).boxed() } else { l.without_time().boxed() }
        }
    }
}

fn make_writer(file: &LogFileConfig) -> io::Result<SizeRotatingWriter> {
    let directory = Path::new(&file.path);
    std::fs::create_dir_all(directory)?;
    SizeRotatingWriter::open(directory.join(LOG_FILE_NAME), file.max_file_size.0, file.max_history)
}

struct SizeRotatingWriter {
    state: Mutex<RotatingState>,
}

struct RotatingState {
    path: PathBuf,
    file: File,
    size: u64,
    max_size: u64,
    max_history: usize,
}

impl SizeRotatingWriter {
    fn open(path: PathBuf, max_size: u64, max_history: usize) -> io::Result<Self> {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        let writer =
            Self { state: Mutex::new(RotatingState { path, file, size, max_size, max_history }) };
        if max_size != 0 && size >= max_size {
            writer.state().rotate()?;
        }
        Ok(writer)
    }

    fn state(&self) -> MutexGuard<'_, RotatingState> {
        self.state.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Write for SizeRotatingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.state();
        if state.max_size != 0
            && state.size != 0
            && state.size.saturating_add(bytes.len() as u64) > state.max_size
        {
            state.rotate()?;
        }
        let written = state.file.write(bytes)?;
        state.size = state.size.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state().file.flush()
    }
}

impl RotatingState {
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        if self.max_history == 0 {
            self.file =
                OpenOptions::new().write(true).truncate(true).create(true).open(&self.path)?;
            self.size = 0;
            return Ok(());
        }
        let _ = std::fs::remove_file(numbered(&self.path, self.max_history));
        for index in (1..self.max_history).rev() {
            let from = numbered(&self.path, index);
            if from.exists() {
                std::fs::rename(from, numbered(&self.path, index + 1))?;
            }
        }
        std::fs::rename(&self.path, numbered(&self.path, 1))?;
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

fn numbered(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    #[derive(Clone)]
    struct Counter(Arc<AtomicUsize>);

    impl<S> Layer<S> for Counter
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, _: &tracing::Event<'_>, _: Context<'_, S>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn config(level: &str) -> LoggingConfig {
        LoggingConfig { level: level.to_owned(), ..LoggingConfig::default() }
    }

    fn events(cfg: LoggingConfig, emit: impl FnOnce()) -> usize {
        let count = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry()
            .with(Counter(Arc::clone(&count)))
            .with(build_filter_from(&cfg));
        tracing::subscriber::with_default(subscriber, emit);
        count.load(Ordering::Relaxed)
    }

    #[test]
    fn debug_level_is_verbose_for_the_workspace() {
        assert_eq!(events(config("debug"), || tracing::debug!(target: "player", "visible")), 1);
        assert_eq!(
            events(config("debug"), || tracing::debug!(target: "sources::http", "visible")),
            1
        );
        assert_eq!(
            events(config("debug"), || tracing::debug!(target: "kairo_core::track", "visible")),
            1
        );
    }

    #[test]
    fn debug_level_keeps_third_party_targets_at_info() {
        let quiet = || {
            assert_eq!(
                events(config("debug"), || tracing::debug!(target: "reqwest::connect", "hidden")),
                0
            );
            assert_eq!(
                events(config("debug"), || tracing::debug!(target: "openmls::schedule", "hidden")),
                0
            );
            assert_eq!(
                events(config("debug"), || tracing::debug!(
                    target: "symphonia_format_isomp4::atoms::stsd",
                    "hidden"
                )),
                0
            );
        };
        quiet();
        assert_eq!(
            events(config("debug"), || tracing::info!(target: "reqwest::connect", "visible")),
            1
        );
    }

    #[test]
    fn a_target_level_reopens_third_party_logging() {
        let mut cfg = config("debug");
        cfg.levels.insert(String::from("reqwest"), String::from("debug"));
        assert_eq!(events(cfg, || tracing::debug!(target: "reqwest::connect", "visible")), 1);
    }

    #[test]
    fn info_level_hides_debug_even_for_the_workspace() {
        assert_eq!(events(config("info"), || tracing::debug!(target: "player", "hidden")), 0);
    }

    #[test]
    fn a_child_override_does_not_silence_its_parent_workspace_target() {
        let mut cfg = config("debug");
        cfg.levels.insert(String::from("kairo_core"), String::from("info"));
        assert_eq!(events(cfg.clone(), || tracing::debug!(target: "kairo", "visible")), 1);
        assert_eq!(events(cfg, || tracing::debug!(target: "kairo_core", "hidden")), 0);
    }

    #[test]
    fn a_suppressed_target_can_be_reopened() {
        assert_eq!(
            events(config("debug"), || tracing::warn!(
                target: "symphonia_format_isomp4::demuxer",
                "hidden"
            )),
            0
        );

        let mut cfg = config("debug");
        cfg.levels.insert(String::from("symphonia_format_isomp4::demuxer"), String::from("warn"));
        assert_eq!(
            events(cfg, || tracing::warn!(
                target: "symphonia_format_isomp4::demuxer",
                "visible"
            )),
            1
        );
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kairo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn size_rotation_keeps_the_configured_history() {
        let dir = temp_dir("size-rotation");
        let path = dir.join(LOG_FILE_NAME);
        let mut writer = SizeRotatingWriter::open(path.clone(), 8, 2).expect("writer");
        for chunk in [b"aaaa", b"bbbb", b"cccc", b"dddd", b"eeee", b"ffff", b"gggg"] {
            writer.write_all(chunk).expect("write");
        }
        writer.flush().expect("flush");

        assert_eq!(std::fs::read_to_string(&path).expect("current"), "gggg");
        assert_eq!(std::fs::read_to_string(numbered(&path, 1)).expect("newest"), "eeeeffff");
        assert_eq!(std::fs::read_to_string(numbered(&path, 2)).expect("older"), "ccccdddd");
        assert!(!numbered(&path, 3).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn zero_size_keeps_one_append_only_file() {
        let dir = temp_dir("no-rotation");
        let path = dir.join(LOG_FILE_NAME);
        let mut writer = SizeRotatingWriter::open(path.clone(), 0, 5).expect("writer");
        writer.write_all(b"one").expect("write");
        writer.write_all(b"two").expect("write");
        writer.flush().expect("flush");

        assert_eq!(std::fs::read_to_string(&path).expect("current"), "onetwo");
        assert!(!numbered(&path, 1).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn zero_history_restarts_the_current_file() {
        let dir = temp_dir("zero-history");
        let path = dir.join(LOG_FILE_NAME);
        let mut writer = SizeRotatingWriter::open(path.clone(), 4, 0).expect("writer");
        writer.write_all(b"aaaa").expect("write");
        writer.write_all(b"bbbb").expect("write");
        writer.flush().expect("flush");

        assert_eq!(std::fs::read_to_string(&path).expect("current"), "bbbb");
        assert!(!numbered(&path, 1).exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
