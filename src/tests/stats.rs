use super::*;
use crate::test_support::app_state;
fn state() -> AppState {
    let mut state = app_state(None);
    state.cores = 4;
    state
}

#[test]
fn memory_maps_to_reference_fields() {
    let m = memory(16384 * 1024, 8192 * 1024);
    assert_eq!(m.allocated, 16384 * 1024);
    assert_eq!(m.free, 8192 * 1024);
    assert_eq!(m.used, m.allocated - m.free);
    assert_eq!(m.reservable, m.allocated);
}

#[test]
fn memory_that_reads_backwards_stays_sane() {
    assert_eq!(memory(100, 200).used, 0);
    assert_eq!(memory(0, 0), Memory::default());
}

#[test]
fn process_load_is_cpu_over_the_core_time_available() {
    // A core busy for the whole window is 1.0 of one core and 0.25 of four.
    assert_eq!(load_fraction(Some((0, 0)), (1_000, 1_000), 1), 1.0);
    assert_eq!(load_fraction(Some((0, 0)), (1_000, 1_000), 4), 0.25);
    assert_eq!(load_fraction(Some((500, 1_000)), (750, 1_500), 1), 0.5);
}

#[test]
fn the_first_reading_and_a_stuck_clock_are_zero_not_panic() {
    assert_eq!(load_fraction(None, (1_000, 1_000), 4), 0.0);
    assert_eq!(load_fraction(Some((1_000, 1_000)), (1_000, 1_000), 4), 0.0);
    assert_eq!(load_fraction(Some((2_000, 1_000)), (1_000, 2_000), 4), 0.0);
    assert_eq!(clamp01(1.5), 1.0);
    assert_eq!(clamp01(-0.5), 0.0);
}

/// The wiring rather than the arithmetic: what the operating system answers for this process.
#[test]
fn metrics_come_from_the_operating_system() {
    let state = state();
    let first = read_metrics(&state);
    assert!(first.total > 0, "the machine's memory is not reported, so the reader is not wired up");
    assert!(first.available <= first.total);
    assert!(first.load_one >= 0.0);
    assert_eq!(
        first.process_load, 0.0,
        "the first reading of a process has no window to divide by"
    );

    let busy = std::time::Instant::now();
    while busy.elapsed() < std::time::Duration::from_millis(500) {}
    let second = read_metrics(&state);
    assert!(
        second.process_load > 0.0,
        "a quarter of a core of busy work in this process went unreported"
    );
}

#[test]
fn stats_frame_keeps_op_and_a_null_frame_stats() {
    let frame = StatsFrame {
        op: "stats",
        frame_stats: None,
        stats: Stats {
            players: 0,
            playing_players: 0,
            uptime: 10,
            memory: Memory::default(),
            cpu: Cpu { cores: 4, system_load: 0.1, lavalink_load: 0.2 },
        },
    };
    let text = serde_json::to_string(&frame).expect("serializable");
    // op first, then the explicit null frameStats, then the flattened node stats.
    assert!(
        text.starts_with("{\"op\":\"stats\",\"frameStats\":null,\"players\":0"),
        "unexpected frame prefix: {text}"
    );
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
    assert_eq!(value["op"], "stats");
    assert!(
        value.get("frameStats").is_some_and(|f| f.is_null()),
        "the socket frame keeps frameStats as null, unlike REST"
    );
    assert_eq!(value["uptime"], 10);
    assert_eq!(value["cpu"]["cores"], 4);
}

#[test]
fn stats_json_has_the_reference_shape() {
    let value = serde_json::to_value(Stats {
        players: 2,
        playing_players: 1,
        uptime: 3000,
        memory: Memory::default(),
        cpu: Cpu { cores: 4, system_load: 0.1, lavalink_load: 0.2 },
    })
    .expect("serializable");
    assert_eq!(value["playingPlayers"], 1);
    assert_eq!(value["memory"]["reservable"], 0);
    assert_eq!(value["cpu"]["systemLoad"], 0.1);
    assert!(value.get("frameStats").is_none(), "frameStats must be omitted");
    assert!(value["memory"].get("total").is_none(), "reference uses allocated, not total");
}
