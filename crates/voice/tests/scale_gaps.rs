use std::time::Duration;

use tokio::time::Instant;
const ON_TIME: Duration = Duration::from_millis(25);
const TOGETHER: Duration = Duration::from_millis(10);

#[derive(Default, Clone, Copy)]
pub(crate) struct Cadence {
    pub(crate) frames: u64,
    pub(crate) shortest: Option<Duration>,
    pub(crate) longest: Option<Duration>,
    pub(crate) late: u64,
    pub(crate) bunched: u64,
}

#[derive(Default)]
pub(crate) struct Watch {
    since: Option<Instant>,
    cadence: Cadence,
}

impl Watch {
    pub(crate) fn record(&mut self, at: Instant) {
        match self.since.replace(at) {
            None => self.cadence.frames += 1,
            Some(previous) => self.cadence.gap(at.saturating_duration_since(previous)),
        }
    }

    pub(crate) fn cadence(&self) -> Cadence {
        self.cadence
    }
}

impl Cadence {
    fn gap(&mut self, gap: Duration) {
        self.frames += 1;
        self.shortest = Some(self.shortest.unwrap_or(gap).min(gap));
        self.longest = Some(self.longest.unwrap_or(gap).max(gap));
        self.late += u64::from(gap >= ON_TIME);
        self.bunched += u64::from(gap < TOGETHER);
    }

    fn merge(&mut self, other: Self) {
        self.frames += other.frames;
        self.shortest = both(self.shortest, other.shortest, Duration::min);
        self.longest = both(self.longest, other.longest, Duration::max);
        self.late += other.late;
        self.bunched += other.bunched;
    }
}

fn both(
    one: Option<Duration>,
    other: Option<Duration>,
    pick: fn(Duration, Duration) -> Duration,
) -> Option<Duration> {
    match (one, other) {
        (Some(a), Some(b)) => Some(pick(a, b)),
        (a, b) => a.or(b),
    }
}

pub(crate) fn spread(every: &[Cadence]) -> (Cadence, u64) {
    let mut all = Cadence::default();
    let mut quietest = 0;
    for (index, cadence) in every.iter().enumerate() {
        all.merge(*cadence);
        if index == 0 || cadence.frames < quietest {
            quietest = cadence.frames;
        }
    }
    (all, quietest)
}
