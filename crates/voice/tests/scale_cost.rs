use std::time::Duration;

use super::FRAME;
use super::gaps::Cadence;
use crate::sender::FrameSource;
#[derive(Default, Clone, Copy)]
pub(crate) struct Cost {
    cpu: u64,
    rss: u64,
}

impl Cost {
    pub(crate) fn now() -> Self {
        let pid = sysinfo::Pid::from_u32(std::process::id());
        let Ok(mut system) = sysinfo::System::new() else {
            return Self::default();
        };
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        match system.process(pid) {
            Some(process) => Self { cpu: process.accumulated_cpu_time(), rss: process.memory() },
            None => Self::default(),
        }
    }

    fn cpu_ms(&self) -> f64 {
        Cost::now().cpu.saturating_sub(self.cpu) as f64
    }

    fn rss_growth(&self) -> u64 {
        Cost::now().rss.saturating_sub(self.rss) / 1024
    }
}

pub(crate) struct Outcome {
    pub(crate) what: &'static str,
    pub(crate) connections: usize,
    pub(crate) window: Duration,
    pub(crate) sent: u64,
    pub(crate) skipped: u64,
    pub(crate) cadence: Cadence,
    pub(crate) quietest: u64,
    pub(crate) cpu: Cost,
    pub(crate) memory: Cost,
}

impl Outcome {
    pub(crate) fn delivered(&self) -> f64 {
        100.0 * self.sent as f64 / self.nominal().max(1) as f64
    }

    fn nominal(&self) -> u64 {
        self.connections as u64 * (self.window.as_millis() / FRAME.as_millis()) as u64
    }

    pub(crate) fn line(&self) -> String {
        let cpu = self.cpu.cpu_ms();
        let rss = self.memory.rss_growth();
        format!(
            "{what:24} {connections:4} x {window:>4.1} s | sent {sent}/{nominal} \
             ({delivered:>5.1} %), arrived {arrived}, skipped {skipped:5}, quietest {quietest:4} | \
             gaps {shortest:>5.1}/{longest:>6.1} ms, {bunched} bunched, {late} late | \
             {cores:.2} cores, {per_frame:.4} ms per frame, {rss} kB more resident \
             (+{each} kB each)",
            what = self.what,
            connections = self.connections,
            window = self.window.as_secs_f64(),
            sent = self.sent,
            nominal = self.nominal(),
            delivered = self.delivered(),
            arrived = self.cadence.frames,
            skipped = self.skipped,
            quietest = self.quietest,
            shortest = self.cadence.shortest.unwrap_or(Duration::ZERO).as_secs_f64() * 1000.0,
            longest = self.cadence.longest.unwrap_or(Duration::ZERO).as_secs_f64() * 1000.0,
            bunched = self.cadence.bunched,
            late = self.cadence.late,
            cores = cpu / (self.window.as_secs_f64() * 1000.0),
            per_frame = cpu / self.sent.max(1) as f64,
            rss = rss,
            each = rss.checked_div(self.connections as u64).unwrap_or(0),
        )
    }
}

pub(crate) struct Speech;

impl FrameSource for Speech {
    fn provide(&mut self, out: &mut [u8]) -> Option<usize> {
        let len = out.len().min(60);
        out[..len].fill(0);
        out[0] = 0x03;
        Some(len)
    }
}
