use super::*;

#[test]
fn close_actions_match_the_retry_table() {
    for code in [1001, 1006, 4000, 4001, 4002, 4003, 4005, 4009, 4012, 4015, 4016, 4020] {
        assert_eq!(close_action(code), CloseAction::Resume, "{code}");
    }
    assert_eq!(close_action(4006), CloseAction::Reidentify);
    for code in [1000, 4004, 4011, 4014, 4021, 4022, 4999] {
        assert_eq!(close_action(code), CloseAction::Fatal, "{code}");
    }
}

#[test]
fn backoff_doubles_within_bands() {
    assert_eq!(backoff_delay(0, 0), Duration::from_millis(250));
    assert_eq!(backoff_delay(0, 250), Duration::from_millis(500));
    assert_eq!(backoff_delay(1, 0), Duration::from_millis(500));
    for attempt in 0..24 {
        for r in [0, 1, 12345, u64::MAX] {
            let d = backoff_delay(attempt, r);
            assert!(
                d >= Duration::from_millis(250) && d <= Duration::from_millis(30_000),
                "attempt {attempt} r {r} -> {d:?}"
            );
        }
    }
    assert_eq!(backoff_delay(20, 0), Duration::from_millis(15_000));
    assert!(reconnect_delay(0) >= Duration::from_millis(250));
}
