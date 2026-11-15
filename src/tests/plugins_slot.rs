use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kairo_plugin_api::{HostError, PluginError};

use super::*;

#[tokio::test]
async fn the_gate_runs_sixteen_calls_and_queues_the_rest() {
    let gate = Arc::new(CallGate::new());
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (release, _) = tokio::sync::watch::channel(false);
    let mut calls = tokio::task::JoinSet::new();

    for _ in 0..MAX_CONCURRENT_CALLS + 4 {
        let gate = Arc::clone(&gate);
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        let mut release = release.subscribe();
        calls.spawn(async move {
            gate.run(async {
                let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(running, Ordering::SeqCst);
                while !*release.borrow() {
                    release.changed().await.expect("release sender");
                }
                active.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), HostError>(())
            })
            .await
            .expect("gate call");
        });
    }

    while active.load(Ordering::SeqCst) < MAX_CONCURRENT_CALLS {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(peak.load(Ordering::SeqCst), MAX_CONCURRENT_CALLS);

    release.send(true).expect("release");
    while calls.join_next().await.is_some() {}
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_call_past_the_deadline_times_out() {
    let gate = CallGate::with_timeout(Duration::from_millis(5));

    let outcome = gate.run(pending::<Result<(), HostError>>()).await;

    assert!(matches!(outcome, Err(CallError::Timeout)));
}

#[test]
fn only_sick_plugin_failures_count() {
    let call = |error| CallError::Host(HostError::Call { operation: "call", error });

    assert!(CallError::Timeout.counted());
    assert!(call(PluginError::fault("broken")).counted());
    assert!(call(PluginError::unavailable("down")).counted());
    assert!(!call(PluginError::common("no result")).counted());
    assert!(!call(PluginError::suspicious("odd request")).counted());
    assert!(CallError::Host(HostError::UnknownMethod { method: "nope".into() }).counted());
    assert!(!CallError::Host(HostError::Shutdown).counted());
}
