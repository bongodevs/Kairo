use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::worker::cpu;
#[test]
fn audio_compute_never_exceeds_the_process_core_budget() {
    let slots = cpu::slot_count();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let handle = runtime.handle().clone();
    let mut workers = Vec::with_capacity(slots * 3);
    for _ in 0..slots * 3 {
        let active = active.clone();
        let peak = peak.clone();
        let handle = handle.clone();
        workers.push(std::thread::spawn(move || {
            cpu::run(&handle, || {
                let current = active.fetch_add(1, Ordering::AcqRel) + 1;
                peak.fetch_max(current, Ordering::AcqRel);
                std::thread::sleep(Duration::from_millis(5));
                active.fetch_sub(1, Ordering::AcqRel);
            });
        }));
    }
    for worker in workers {
        worker.join().expect("worker");
    }
    assert!(peak.load(Ordering::Acquire) <= slots);
    assert_eq!(active.load(Ordering::Acquire), 0);
}

#[test]
#[ignore = "measures uncontended audio-budget acquisition in release mode"]
fn audio_budget_acquisition_has_a_measured_cost() {
    const RUNS: u32 = 100_000;
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let started = std::time::Instant::now();
    for _ in 0..RUNS {
        cpu::run(runtime.handle(), || std::hint::black_box(()));
    }
    let nanos = started.elapsed().as_nanos() / u128::from(RUNS);
    eprintln!("uncontended audio CPU permit: {nanos} ns");
    assert!(nanos < 20_000, "CPU permit cost exceeded 20 µs");
}
