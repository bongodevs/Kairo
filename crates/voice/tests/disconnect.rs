use super::*;

fn established() -> Retry {
    let mut retry = Retry::default();
    retry.saw_ready();
    retry
}

#[test]
fn a_clean_or_permanent_close_is_a_report() {
    let mut retry = established();
    for code in [1000, 4004, 4011, 4014, 4021, 4022, 4999] {
        assert_eq!(retry.attempt(code, 5), None, "{code}");
        assert_eq!(retry.attempts(), 0, "{code} spends no attempt");
    }
}

#[test]
fn a_transient_close_resumes_the_session() {
    for code in [1001, 1006, 4000, 4015, 4020, CLOSE_RECONNECT] {
        let mut retry = established();
        assert_eq!(retry.attempt(code, 9), Some(Intro::Resume(9)), "{code}");
    }
}

#[test]
fn three_attempts_is_the_whole_budget_for_one_outage() {
    let mut retry = established();
    for _ in 0..2 {
        assert_eq!(retry.attempt(4015, 1), Some(Intro::Resume(1)));
    }
    assert_eq!(retry.attempts(), 2);
    assert_eq!(retry.attempt(4015, 1), Some(Intro::Resume(1)), "the third");
    assert_eq!(retry.attempt(4015, 1), None, "the fourth is what gets reported");
}

#[test]
fn a_proven_session_pays_the_budget_back() {
    let mut retry = established();
    for _ in 0..3 {
        assert!(retry.attempt(4015, 1).is_some());
    }
    assert_eq!(retry.attempt(4015, 1), None, "the budget is spent");
    retry.described();
    assert_eq!(retry.attempts(), 0, "a session that proved itself starts over");
    assert!(retry.attempt(4015, 1).is_some(), "and can pay again");
}

#[test]
fn a_forgotten_session_identifies_once_and_then_reports() {
    let mut retry = established();
    assert_eq!(
        retry.attempt(CLOSE_SESSION_LOST, 7),
        Some(Intro::Identify),
        "a resume would only be refused again"
    );
    assert_eq!(
        retry.attempt(CLOSE_SESSION_LOST, 7),
        None,
        "the session is gone, so this is the report, and it says the session is lost"
    );
}

#[test]
fn a_session_that_never_had_a_ready_cannot_be_resumed() {
    let mut retry = Retry::default();
    assert_eq!(retry.attempt(4015, 3), Some(Intro::Identify), "nothing to resume, so identify");
    assert_eq!(retry.attempt(CLOSE_SESSION_LOST, 3), None);
}

#[test]
fn a_new_ready_makes_the_session_resumable_again() {
    let mut retry = established();
    assert!(matches!(retry.attempt(CLOSE_SESSION_LOST, 1), Some(Intro::Identify)));
    retry.described();
    retry.saw_ready();
    assert_eq!(retry.attempt(4015, 2), Some(Intro::Resume(2)));
}

#[test]
fn only_losing_the_session_needs_a_fresh_voice_update() {
    assert!(Disconnect::local(CLOSE_SESSION_LOST, "").session_lost());
    for code in [1000, 1006, 4000, 4015, CLOSE_RECONNECT] {
        assert!(
            !Disconnect::remote(code, String::new()).session_lost(),
            "{code} is a close, not a lost session"
        );
    }
}

#[test]
fn an_absent_reason_is_empty_never_missing() {
    let close = Disconnect::remote(4015, String::new());
    assert_eq!(close.reason, "");
    assert!(close.by_remote);
    assert!(!Disconnect::abnormal("gone").by_remote);
}
