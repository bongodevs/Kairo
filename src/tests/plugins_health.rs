use super::*;

#[test]
fn five_consecutive_failures_close_the_source() {
    let health = SourceHealth::new();

    for _ in 1..FAILURE_LIMIT {
        health.failed();
        assert!(health.available());
    }
    health.failed();

    assert!(!health.available());
}

#[test]
fn a_success_clears_the_failure_count() {
    let health = SourceHealth::new();
    for _ in 0..FAILURE_LIMIT - 1 {
        health.failed();
    }

    health.succeeded();
    for _ in 0..FAILURE_LIMIT - 1 {
        health.failed();
    }

    assert!(health.available());
}

#[test]
fn the_window_expires_and_one_more_failure_closes_it_again() {
    let health = SourceHealth::new();
    for _ in 0..FAILURE_LIMIT {
        health.failed();
    }
    assert!(!health.available());

    health.expire();
    assert!(health.available());

    health.failed();
    assert!(!health.available());
}

#[test]
fn a_closed_source_reports_the_fixed_busy_message() {
    let health = SourceHealth::new();
    for _ in 0..FAILURE_LIMIT {
        health.failed();
    }

    let error = health.gate().expect_err("closed source");

    assert_eq!(error.to_string(), BUSY_MESSAGE);
    assert_eq!(error.severity(), ExceptionSeverity::Common);
}
