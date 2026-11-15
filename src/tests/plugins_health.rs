use super::*;

fn close(health: &SourceHealth) {
    for _ in 0..FAILURE_LIMIT {
        health.enter().expect("source is up").failed();
    }
}

#[test]
fn five_consecutive_failures_close_the_source() {
    let health = SourceHealth::new();

    for _ in 1..FAILURE_LIMIT {
        health.enter().expect("source is up").failed();
    }
    health.enter().expect("source is up").failed();

    let error = health.enter().err().expect("source is closed");
    assert!(is_busy(&error));
}

#[test]
fn a_success_clears_the_failure_count() {
    let health = SourceHealth::new();
    for _ in 0..FAILURE_LIMIT - 1 {
        health.enter().expect("source is up").failed();
    }

    health.enter().expect("source is up").succeeded();
    for _ in 0..FAILURE_LIMIT - 1 {
        health.enter().expect("source is up").failed();
    }

    health.enter().expect("source is still up").succeeded();
}

#[test]
fn an_expired_window_allows_exactly_one_probe() {
    let health = SourceHealth::new();
    close(&health);
    health.expire();

    let probe = health.enter().expect("first caller probes");
    let error = health.enter().err().expect("second caller waits");
    assert!(is_busy(&error));

    probe.succeeded();
    health.enter().expect("probe success reopens the source").succeeded();
}

#[test]
fn a_failed_probe_closes_the_source_again() {
    let health = SourceHealth::new();
    close(&health);
    health.expire();

    health.enter().expect("probe").failed();

    let error = health.enter().err().expect("source is closed again");
    assert!(is_busy(&error));
}

#[test]
fn a_cancelled_probe_does_not_leave_the_source_probing() {
    let health = SourceHealth::new();
    close(&health);
    health.expire();

    drop(health.enter().expect("probe"));

    let error = health.enter().err().expect("source is closed again");
    assert!(is_busy(&error));
}

#[test]
fn a_closed_source_reports_the_fixed_busy_message() {
    let health = SourceHealth::new();
    close(&health);

    let error = health.enter().err().expect("source is closed");

    assert_eq!(error.to_string(), BUSY_MESSAGE);
    assert_eq!(error.severity(), ExceptionSeverity::Common);
}
