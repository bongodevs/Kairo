use super::*;
use crate::test_rss::rss;
const PLAYERS: usize = 1_000;

#[test]
#[ignore = "creates 1000 active echo filters to measure resident memory"]
fn a_thousand_echo_filters_have_a_measured_bound() {
    let before = rss();
    let filters = active_filters();
    let growth = rss().saturating_sub(before);
    let per_player = growth / PLAYERS as u64;
    eprintln!(
        "{PLAYERS} echo filters: {} MiB total, {} KiB/player",
        growth / (1024 * 1024),
        per_player / 1024,
    );
    assert!(per_player <= 160 * 1024, "a 0.3-second echo exceeded its 160 KiB measured budget");
    drop(filters);
    let retained_once = rss().saturating_sub(before);
    let filters = active_filters();
    drop(filters);
    let retained_twice = rss().saturating_sub(before);
    assert!(
        retained_twice <= retained_once.saturating_add(16 * 1024 * 1024),
        "a second lifecycle grew RSS by more than 16 MiB"
    );
}

fn active_filters() -> Vec<Echo> {
    let config = EchoConfig { length_seconds: 0.3, decay: 0.5 };
    let mut filters = (0..PLAYERS).map(|_| Echo::new(config).expect("echo")).collect::<Vec<_>>();
    let mut block = [0.0; crate::FRAME_SAMPLES * 2];
    for filter in &mut filters {
        for _ in 0..15 {
            filter.process(&mut block);
        }
    }
    filters
}
