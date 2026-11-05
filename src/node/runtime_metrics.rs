//! Tokio runtime metrics, logged every 5 s.
//!
//! Stable metrics only: worker count, alive tasks, global queue depth, and
//! per-worker busy % derived from cumulative `worker_total_busy_duration`
//! deltas (64-bit stable, no flag needed).
//!
//! Deliberately not exported: the blocking-pool gauges (`num_blocking_threads`,
//! `blocking_queue_depth`) require `--cfg tokio_unstable` at tokio compile time.
//! That flag has to come from global RUSTFLAGS/`.cargo/config.toml`, which
//! silently drops out from under any user with their own RUSTFLAGS set and
//! forces full-dependency rebuilds for everyone. The pool (default 512 threads
//! serving a loader pool of 10 plus a few one-off blocking calls) is not the
//! suspected bottleneck; worker busy % plus global queue depth answer the
//! saturation question. Revisit if those two ever implicate the blocking pool.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::node::tasks::TASKS;
use crate::utils::locked;

// Last (sample time, per-worker busy totals). First tick only stores a baseline.
static LAST: Mutex<Option<(Instant, Vec<Duration>)>> = Mutex::new(None);

/// Log runtime load every 5 s. Call once from `AppState::new` (inside the runtime).
pub fn spawn_runtime_metrics_task() {
    TASKS.add("runtime_metrics", Duration::from_secs(5), || async {
        let m = tokio::runtime::Handle::current().metrics();
        let workers = m.num_workers();
        let now = Instant::now();
        let current: Vec<Duration> = (0..workers)
            .map(|w| m.worker_total_busy_duration(w))
            .collect();

        let (max_busy, avg_busy, saturated) = {
            let mut last = locked(&LAST);
            match last.as_ref() {
                Some((at, prev)) if prev.len() == current.len() => {
                    let elapsed = now.duration_since(*at).as_secs_f64().max(f64::EPSILON);
                    let pcts: Vec<f64> = current
                        .iter()
                        .zip(prev.iter())
                        .map(|(c, p)| c.saturating_sub(*p).as_secs_f64() / elapsed * 100.0)
                        .collect();
                    let max = pcts.iter().cloned().fold(0.0f64, f64::max);
                    let avg = pcts.iter().sum::<f64>() / pcts.len().max(1) as f64;
                    *last = Some((now, current));
                    (
                        format!("{max:.1}"),
                        format!("{avg:.1}"),
                        max >= 70.0 || m.global_queue_depth() > 0,
                    )
                }
                _ => {
                    *last = Some((now, current));
                    ("baseline".to_string(), "baseline".to_string(), false)
                }
            }
        };

        // Log quietly: at DEBUG on a healthy node so the console stays readable,
        // at INFO when workers look saturated (busy) or work starts queueing.
        // The same numbers are always available from Prometheus (`kairo_runtime_*`).
        if saturated {
            tracing::info!(
                workers,
                alive_tasks = m.num_alive_tasks(),
                global_queue = m.global_queue_depth(),
                max_busy_pct = max_busy,
                avg_busy_pct = avg_busy,
                "runtime metrics (saturated)"
            );
        } else {
            tracing::debug!(
                workers,
                alive_tasks = m.num_alive_tasks(),
                global_queue = m.global_queue_depth(),
                max_busy_pct = max_busy,
                avg_busy_pct = avg_busy,
                "runtime metrics"
            );
        }
    });
}
