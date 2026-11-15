#[cfg(test)]
#[path = "../../tests/decode_trim.rs"]
mod tests;

use symphonia::core::units::{TimeBase, Timestamp};

use kairo_core::Millis;

use crate::clock::frames_to_nanos;

pub(super) fn packet_nanos(
    time_base: Option<TimeBase>,
    start: Timestamp,
    pts: Timestamp,
) -> Option<i128> {
    let relative = pts.saturating_delta(start).get();
    Some(time_base?.calc_time(Timestamp::new(relative))?.as_nanos())
}

pub(super) fn millis_from_nanos(nanos: u128) -> Millis {
    let millis = nanos / 1_000_000;
    Millis::new(u64::try_from(millis).unwrap_or(u64::MAX))
}

pub(super) fn after_block(position_nanos: u128, frames: usize, sample_rate: u32) -> u128 {
    position_nanos.saturating_add(frames_to_nanos(frames as u128, sample_rate))
}

fn frames_until(target: u128, position: u128, sample_rate: u32) -> usize {
    if position >= target {
        return 0;
    }
    let nanos = target - position;
    let frames = (nanos * u128::from(sample_rate)).div_ceil(1_000_000_000);
    usize::try_from(frames).unwrap_or(usize::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrimmedWindow {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) frames: usize,
    pub(super) position_nanos: u128,
}

pub(super) struct TrimInput {
    pub(super) decoded_frames: usize,
    pub(super) sample_rate: u32,
    pub(super) packet_position_nanos: Option<i128>,
    pub(super) fallback_nanos: u128,
    pub(super) delay_start: usize,
    pub(super) packet_start: usize,
    pub(super) packet_end: usize,
    pub(super) packet_trimmed_by_decoder: bool,
}

pub(super) fn decoded_window(input: TrimInput, seek: &mut SeekState) -> TrimmedWindow {
    let slice_packet_start = if input.packet_trimmed_by_decoder { 0 } else { input.packet_start };
    let slice_packet_end = if input.packet_trimmed_by_decoder { 0 } else { input.packet_end };
    let intrinsic_start = input.delay_start.max(slice_packet_start).min(input.decoded_frames);
    let intrinsic_end = slice_packet_end.min(input.decoded_frames - intrinsic_start);
    let playable = input.decoded_frames - intrinsic_start - intrinsic_end;
    let base_position = presented_position(&input);
    let seek_start = seek.trim(base_position, playable, input.sample_rate);
    let start = intrinsic_start + seek_start;
    let frames = input.decoded_frames - start - intrinsic_end;
    let position_nanos =
        base_position.saturating_add(frames_to_nanos(seek_start as u128, input.sample_rate));
    TrimmedWindow { start, end: intrinsic_end, frames, position_nanos }
}

pub(super) fn packet_position(position_nanos: Option<i128>, fallback_nanos: u128) -> u128 {
    match position_nanos {
        Some(position) => u128::try_from(position.max(0)).unwrap_or(u128::MAX),
        None => fallback_nanos,
    }
}

fn presented_position(input: &TrimInput) -> u128 {
    let Some(raw) = input.packet_position_nanos else {
        return input.fallback_nanos;
    };
    let packet_trim = signed_frames(input.packet_start, input.sample_rate);
    let mut position = raw.saturating_add(packet_trim);
    if position < 0 {
        let delay = signed_frames(input.delay_start, input.sample_rate);
        position = position.saturating_add(delay.min(position.saturating_neg()));
    }
    u128::try_from(position.max(0)).unwrap_or(u128::MAX)
}

fn signed_frames(frames: usize, sample_rate: u32) -> i128 {
    i128::try_from(frames_to_nanos(frames as u128, sample_rate)).unwrap_or(i128::MAX)
}

#[derive(Default)]
pub(super) struct SeekState {
    target_nanos: Option<u128>,
}

impl SeekState {
    pub(super) fn trim(&mut self, position: u128, frames: usize, sample_rate: u32) -> usize {
        let Some(target) = self.target_nanos else {
            return 0;
        };
        let trim = frames_until(target, position, sample_rate).min(frames);
        if trim < frames || position >= target {
            self.target_nanos = None;
        }
        trim
    }

    pub(super) fn allows_passthrough(&mut self, position: u128) -> bool {
        let Some(target) = self.target_nanos else {
            return true;
        };
        if position < target {
            return false;
        }
        self.target_nanos = None;
        true
    }

    pub(super) fn reset(&mut self, position: Millis) {
        self.target_nanos = Some(u128::from(position.get()) * 1_000_000);
    }
}
