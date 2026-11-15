use super::*;

#[test]
fn previous_minute_counts_sent_and_nulled_separately() {
    let counter = FrameCounter::new();
    counter.start_at(9 * MINUTE_MS);
    for _ in 0..12 {
        counter.record_at(true, 10 * MINUTE_MS + 1);
    }
    for _ in 0..3 {
        counter.record_at(false, 10 * MINUTE_MS + 2);
    }
    assert_eq!(counter.window_at(11 * MINUTE_MS + 1), Some(FrameWindow { sent: 12, nulled: 3 }));
}

#[test]
fn only_a_gap_over_one_hundred_milliseconds_resets_continuity() {
    let counter = FrameCounter::new();
    counter.start_at(9 * MINUTE_MS);
    counter.end_at(10 * MINUTE_MS + 1_000);
    counter.start_at(10 * MINUTE_MS + 1_100);
    assert!(counter.window_at(11 * MINUTE_MS + 1).is_some());

    counter.end_at(11 * MINUTE_MS + 2_000);
    counter.start_at(11 * MINUTE_MS + 2_101);
    assert!(counter.window_at(12 * MINUTE_MS + 1).is_none());
}

#[test]
fn a_missing_previous_bucket_is_a_full_deficit_not_missing_data() {
    let counter = FrameCounter::new();
    counter.start_at(9 * MINUTE_MS);
    assert_eq!(counter.window_at(11 * MINUTE_MS), Some(FrameWindow { sent: 0, nulled: 0 }));
}

#[test]
fn frame_stats_are_integer_averages_per_usable_player() {
    assert_eq!(summarize(&[]), None);
    let stats = summarize(&[
        FrameWindow { sent: 2_900, nulled: 50 },
        FrameWindow { sent: 2_800, nulled: 100 },
    ]);
    assert_eq!(stats, Some(FrameStats { sent: 2_850, nulled: 75, deficit: 75 }));
    assert_eq!(
        serde_json::to_value(stats).expect("frame stats JSON"),
        serde_json::json!({"sent": 2850, "nulled": 75, "deficit": 75})
    );
}

#[test]
#[ignore = "measures the atomic frame-accounting path in release mode"]
fn frame_accounting_has_a_measured_cost() {
    let counter = FrameCounter::new();
    let iterations = 1_000_000_u32;
    let started = std::time::Instant::now();
    for sent in (0..iterations).map(|index| index & 1 == 0) {
        std::hint::black_box(&counter).record(sent);
    }
    let ns = started.elapsed().as_nanos() / u128::from(iterations);
    eprintln!("frame accounting: {ns} ns/pull");
}
