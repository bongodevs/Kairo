use super::Millis;
use super::SeekState;
use super::{TrimInput, TrimmedWindow, decoded_window};
fn trim(input: TrimInput) -> TrimmedWindow {
    decoded_window(input, &mut SeekState::default())
}

#[test]
fn manual_packet_trims_bound_the_returned_window() {
    let window = trim(TrimInput {
        decoded_frames: 10,
        sample_rate: 1_000,
        packet_position_nanos: Some(0),
        fallback_nanos: 0,
        delay_start: 0,
        packet_start: 2,
        packet_end: 3,
        packet_trimmed_by_decoder: false,
    });
    assert_eq!(window, TrimmedWindow { start: 2, end: 3, frames: 5, position_nanos: 2_000_000 });
}

#[test]
fn a_gapless_decoder_is_not_trimmed_twice() {
    let window = trim(TrimInput {
        decoded_frames: 5,
        sample_rate: 1_000,
        packet_position_nanos: Some(-2_000_000),
        fallback_nanos: 0,
        delay_start: 0,
        packet_start: 2,
        packet_end: 3,
        packet_trimmed_by_decoder: true,
    });
    assert_eq!(window.start, 0);
    assert_eq!(window.end, 0);
    assert_eq!(window.frames, 5);
    assert_eq!(window.position_nanos, 0);
}

#[test]
fn track_delay_removes_samples_without_shifting_a_zero_based_timeline() {
    let window = trim(TrimInput {
        decoded_frames: 10,
        sample_rate: 1_000,
        packet_position_nanos: Some(0),
        fallback_nanos: 0,
        delay_start: 2,
        packet_start: 0,
        packet_end: 0,
        packet_trimmed_by_decoder: false,
    });
    assert_eq!(window.start, 2);
    assert_eq!(window.frames, 8);
    assert_eq!(window.position_nanos, 0);
}

#[test]
fn seek_trim_starts_at_the_requested_valid_sample() {
    let mut seek = SeekState::default();
    seek.reset(Millis::new(5));
    let window = decoded_window(
        TrimInput {
            decoded_frames: 10,
            sample_rate: 1_000,
            packet_position_nanos: Some(0),
            fallback_nanos: 0,
            delay_start: 2,
            packet_start: 0,
            packet_end: 0,
            packet_trimmed_by_decoder: false,
        },
        &mut seek,
    );
    assert_eq!(window.start, 7);
    assert_eq!(window.frames, 3);
    assert_eq!(window.position_nanos, 5_000_000);
}
