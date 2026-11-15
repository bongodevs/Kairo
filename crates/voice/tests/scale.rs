//! The V13 measurement pieces that are about the schedule rather than one connection: this
//! process's CPU and memory as the operating system reports them, arrival-gap accounting, loopback
//! media sinks, and the two scheduler shapes priced against each other. Declared only for tests,
//! and its tests are `#[ignore]`d, so the gate never pays for them; the question they answer is
//! docs/reference/pacing-scale.md.

use std::time::Duration;

#[path = "scale_clocks.rs"]
mod clocks;
#[path = "scale_cost.rs"]
mod cost;
#[path = "scale_gaps.rs"]
mod gaps;
#[path = "scale_sinks.rs"]
mod sinks;

pub(crate) use cost::{Cost, Outcome, Speech};
pub(crate) use gaps::spread;
pub(crate) use sinks::Receivers;
pub(crate) const FRAME: Duration = Duration::from_millis(20);
pub(crate) const KEEP: f64 = 99.0;
