use super::*;
use crate::udp::rtp::{RTP_HEADER_LEN, TIMESTAMP_INCREMENT};
fn started() -> Pacing {
    let mut pacing = Pacing::new();
    pacing.arm();
    pacing
}

async fn stalled(frames: u64) {
    tokio::time::advance(FRAME_INTERVAL * u32::try_from(frames).expect("small")).await;
}

#[tokio::test(start_paused = true)]
async fn arming_owes_the_first_frame_now() {
    let mut pacing = Pacing::new();
    assert_eq!(pacing.deadline(), None, "at rest there is no deadline at all");
    let now = Instant::now();
    pacing.arm();
    assert_eq!(pacing.deadline(), Some(now), "the first frame is owed now");
    let armed = pacing.deadline();
    pacing.arm();
    assert_eq!(pacing.deadline(), armed, "re-arming never restarts the clock");
}

#[tokio::test(start_paused = true)]
async fn the_deadline_moves_by_whole_frames_never_by_the_wake_delay() {
    let mut pacing = started();
    let first = pacing.deadline().expect("armed");
    for frame in 1..=15u32 {
        tokio::time::advance(FRAME_INTERVAL + Duration::from_millis(3)).await;
        pacing.advance();
        assert_eq!(pacing.deadline(), Some(first + FRAME_INTERVAL * frame));
    }
    assert_eq!(pacing.stats().skipped, 0, "a 3 ms jitter is not a stall");
}

#[tokio::test(start_paused = true)]
async fn a_stall_is_skipped_and_counted_never_chased() {
    let mut pacing = started();
    let due = pacing.deadline().expect("armed");
    let stalled_for = MAX_FRAMES_BEHIND + 2;
    stalled(stalled_for).await;
    pacing.advance();
    assert_eq!(
        pacing.deadline(),
        Some(due + FRAME_INTERVAL * u32::try_from(stalled_for + 1).expect("small")),
        "restart from now plus one frame: the missed frames are gone, not queued"
    );
    let stats = pacing.stats();
    assert_eq!(stats.sent, 0);
    assert_eq!(
        stats.skipped,
        stalled_for - 1,
        "every frame the schedule was behind is counted as skipped"
    );

    let mut pacing = started();
    let due = pacing.deadline().expect("armed");
    stalled(MAX_FRAMES_BEHIND).await;
    for frame in 1..=MAX_FRAMES_BEHIND as u32 {
        pacing.advance();
        assert_eq!(pacing.deadline(), Some(due + FRAME_INTERVAL * frame));
    }
    assert_eq!(pacing.stats().skipped, 0, "nothing was given up on");
}

#[tokio::test(start_paused = true)]
async fn the_silence_tail_is_exactly_five_frames() {
    let mut pacing = started();
    assert!(!pacing.take_tail(), "a source that never produced owes no tail");
    pacing.produced();
    for _ in 0..SILENCE_TAIL {
        assert!(pacing.take_tail());
    }
    assert!(!pacing.take_tail(), "the tail is bounded");

    pacing.produced();
    pacing.produced();
    pacing.take_tail();
    for _ in 1..SILENCE_TAIL {
        assert!(pacing.take_tail());
    }
    assert!(!pacing.take_tail());
}

#[tokio::test(start_paused = true)]
async fn stopping_clears_the_clock_and_the_tail() {
    let mut pacing = started();
    pacing.produced();
    pacing.take_tail();
    pacing.stop();
    assert_eq!(pacing.deadline(), None);
    assert!(!pacing.take_tail(), "no half-spent tail survives");
    pacing.arm();
    assert_eq!(pacing.deadline(), Some(Instant::now()), "re-arm starts fresh");
}

#[test]
fn counters_separate_sent_from_skipped() {
    let mut pacing = Pacing::new();
    pacing.written();
    pacing.written();
    pacing.skip();
    assert_eq!(pacing.stats(), FrameStats { sent: 2, skipped: 1 });
}

#[test]
fn media_buffers_cover_the_largest_dave_datagram_up_front() {
    let media = Media::new(7);
    assert!(media.encrypted.capacity() >= MAX_DATAGRAM_BYTES);
    assert!(media.sealed.capacity() >= MAX_DATAGRAM_BYTES);
}

#[test]
fn a_rebound_stream_keeps_its_counters_and_loses_its_deadline() {
    let mut media = Media::new(7);
    media.pacing.produced();
    media.pacing.arm();
    let mut header = [0u8; RTP_HEADER_LEN];
    media.rtp.next_header(&mut header, false);
    media.rebind(42);
    assert_eq!(
        media.pacing.deadline(),
        None,
        "the frames the dead session owed are not owed again"
    );
    assert!(!media.pacing.take_tail(), "nor its silence");
    media.rtp.next_header(&mut header, false);
    assert_eq!(
        u32::from_be_bytes(header[8..12].try_into().expect("four bytes")),
        42,
        "the new READY's ssrc keys the stream from here"
    );
    assert_eq!(
        u32::from_be_bytes(header[4..8].try_into().expect("four bytes")),
        TIMESTAMP_INCREMENT,
        "nor does its audio time"
    );
}
