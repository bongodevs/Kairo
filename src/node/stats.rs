//! The node's own resource usage, as reported by `/v4/stats` and the periodic stats event.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use perf_monitor::cpu::ProcessStat;

use crate::protocol::stats::{Cpu, Memory};
use crate::utils::locked;

// Sampling costs a syscall per call and clients ask for stats far more often than the numbers move.
const CPU_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

static CPU: Mutex<Option<CpuState>> = Mutex::new(None);

fn cores() -> i32 {
    perf_monitor::cpu::processor_numbers()
        .map(|count| count as i32)
        .unwrap_or(1)
}

// Linux reports process memory in pages; everywhere else the counts are already bytes.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn memory_scale() -> u64 {
    static PAGE_SIZE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *PAGE_SIZE.get_or_init(|| match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
        size if size > 0 => size as u64,
        _ => 4096,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn memory_scale() -> u64 {
    1
}

fn resident_memory() -> u64 {
    perf_monitor::mem::get_process_memory_info()
        .map(|info| info.resident_set_size * memory_scale())
        .unwrap_or(0)
}

// Sample allocator committed memory from the OS rather than virtual address space.
fn committed_memory() -> u64 {
    let mut fields = [0usize; 8];
    let [elapsed, user, system, rss, peak_rss, commit, peak_commit, faults] = &mut fields;
    unsafe {
        libmimalloc_sys::mi_process_info(
            elapsed,
            user,
            system,
            rss,
            peak_rss,
            commit,
            peak_commit,
            faults,
        );
    }
    fields[5] as u64
}

// The host's RAM, so a container with a memory limit advertises a ceiling it never gets.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn total_ram() -> u64 {
    match unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) } {
        pages if pages > 0 => pages as u64 * memory_scale(),
        _ => 0,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn total_ram() -> u64 {
    0
}

/// Process memory: `used` is the resident set rather than the Rust heap, `allocated` what the
/// allocator holds committed, `free` the committed part that is not resident, `reservable` the
/// physical ceiling.
pub fn memory() -> Memory {
    let resident = resident_memory();
    // A commit smaller than the resident set means the resident pages the allocator does not own
    // (the binary, thread stacks) outweigh its own; clamp so `free` cannot go negative.
    let allocated = committed_memory().max(resident);
    Memory {
        free: (allocated - resident) as i64,
        used: resident as i64,
        allocated: allocated as i64,
        reservable: total_ram().max(allocated) as i64,
    }
}

#[cfg(unix)]
fn system_load() -> Option<f64> {
    let mut avg = [0.0f64; 1];
    if unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) } != 1 {
        return None;
    }
    Some(avg[0] / cores() as f64)
}

#[cfg(not(unix))]
fn system_load() -> Option<f64> {
    None
}

fn ratio(load: f64) -> f64 {
    if load.is_finite() {
        load.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

struct CpuState {
    process: Option<ProcessStat>,
    cached: Cpu,
    refreshed: Instant,
}

impl CpuState {
    fn new() -> Self {
        Self {
            process: ProcessStat::cur().ok(),
            cached: Cpu {
                cores: cores(),
                // Host load reads correctly from the first sample; ours needs a window to elapse.
                system_load: ratio(system_load().unwrap_or(0.0)),
                lavalink_load: 0.0,
            },
            refreshed: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        let lavalink_load = self
            .process
            .as_mut()
            .and_then(|stat| stat.cpu().ok())
            .map(|usage| usage / cores() as f64)
            .unwrap_or(0.0);
        self.cached = Cpu {
            cores: cores(),
            system_load: ratio(system_load().unwrap_or(lavalink_load)),
            lavalink_load: ratio(lavalink_load),
        };
        self.refreshed = Instant::now();
    }
}

/// Open file descriptor count on Linux via `/proc/self/fd`.
///
/// Returns `None` off Linux or when the directory cannot be read. Cheap enough
/// for a 30 s monitor tick: one `readdir` of a few thousand entries. This is the
/// gauge that decides the FD-exhaustion theory: if `fd_count` is far below the
/// limit while HTTP still fails, the limit was never the cause.
#[cfg(target_os = "linux")]
pub fn fd_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.count())
}

#[cfg(not(target_os = "linux"))]
pub fn fd_count() -> Option<usize> {
    None
}

/// Current `RLIMIT_NOFILE` soft limit, or `None` when it cannot be read.
#[cfg(unix)]
pub fn fd_limit() -> Option<u64> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } == 0 {
        Some(lim.rlim_cur)
    } else {
        None
    }
}

#[cfg(not(unix))]
pub fn fd_limit() -> Option<u64> {
    None
}

/// Host and process CPU load, resampled at most once per refresh interval.
pub fn cpu() -> Cpu {
    let mut guard = locked(&CPU);
    let state = guard.get_or_insert_with(CpuState::new);
    if state.refreshed.elapsed() >= CPU_REFRESH_INTERVAL {
        state.refresh();
    }
    state.cached
}

#[cfg(test)]
mod tests {
    use super::*;

    // Assert committed memory reflects physical commit rather than reserved arena space.
    #[test]
    fn memory_reports_commit_rather_than_address_space() {
        let memory = memory();
        assert!(memory.used > 0, "a running process has a resident set");
        assert!(memory.allocated >= memory.used);
        assert_eq!(memory.free, memory.allocated - memory.used);
        assert!(memory.reservable >= memory.allocated);
        // A test binary commits single-digit MB. Two gibibytes is one arena reservation.
        assert!(
            memory.allocated < 2 * 1024 * 1024 * 1024,
            "allocated looks like address space: {}",
            memory.allocated
        );
    }
}
