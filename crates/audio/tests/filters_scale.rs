use super::{Timescale, TimescaleConfig};
use crate::test_rss::rss;
const PLAYERS: usize = 1_000;

#[test]
#[ignore = "creates 1000 active timescale filters to measure resident memory"]
fn a_thousand_timescale_filters_have_a_measured_bound() {
    let before = rss();
    let filters = active_filters();
    let growth = rss().saturating_sub(before);
    let per_player = growth / PLAYERS as u64;
    eprintln!(
        "{PLAYERS} timescale filters: {} MiB total, {} KiB/player",
        growth / (1024 * 1024),
        per_player / 1024,
    );
    assert!(per_player <= 768 * 1024, "timescale state exceeded its 768 KiB measured budget");
    drop(filters);
    let retained_once = rss().saturating_sub(before);

    let filters = active_filters();
    drop(filters);
    let retained_twice = rss().saturating_sub(before);
    eprintln!(
        "after two create/drop cycles: {} MiB then {} MiB retained",
        retained_once / (1024 * 1024),
        retained_twice / (1024 * 1024),
    );
    assert!(
        retained_twice <= retained_once.saturating_add(16 * 1024 * 1024),
        "a second lifecycle grew RSS by more than 16 MiB"
    );
}

fn active_filters() -> Vec<Timescale> {
    let config = TimescaleConfig { speed: 1.25, ..TimescaleConfig::default() };
    (0..PLAYERS).map(|_| Timescale::new(config).expect("timescale")).collect()
}
