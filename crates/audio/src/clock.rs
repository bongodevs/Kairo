use kairo_core::Millis;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const NANOS_PER_MILLI: u128 = 1_000_000;

#[inline]
pub(crate) fn frames_to_nanos(frames: u128, sample_rate: u32) -> u128 {
    frames * NANOS_PER_SECOND / u128::from(sample_rate)
}

#[inline]
pub(crate) fn add_frames(position: Millis, frames: u128, sample_rate: u32) -> Millis {
    let millis = frames_to_nanos(frames, sample_rate) / NANOS_PER_MILLI;
    Millis::new(position.get().saturating_add(u64::try_from(millis).unwrap_or(u64::MAX)))
}
