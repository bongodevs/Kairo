use super::*;
use crate::audio::{FRAME_SAMPLES, MAX_PACKET_BYTES};
use crate::decode::Millis;
use crate::opus::EncodedFrame;
use crate::volume::Volume;
#[tokio::test]
async fn consumer_reports_the_frame_source_end_and_volume_that_reached_output() {
    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    let control = producer.control();
    let encoded = EncodedFrame {
        packet: &[7; 32],
        position: Millis::new(40),
        next_position: Millis::new(75),
        pcm_frames: FRAME_SAMPLES,
        volume: 50,
    };
    producer.send(BufferedFrame::from_encoded(&encoded).expect("frame")).await.expect("send");
    assert!(matches!(
        consumer.read(&mut [0; MAX_PACKET_BYTES]).expect("read"),
        BufferRead::Frame(_)
    ));
    assert_eq!(control.output_state(), Some((Millis::new(75), Volume::new(50))));
    assert_eq!(control.position(), Some(Millis::new(40)));
}

#[tokio::test]
async fn every_consumer_request_refreshes_the_cleanup_clock() {
    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    let control = producer.control();
    producer.begin_track();
    assert!(control.take_request());
    assert!(!control.take_request());

    assert_eq!(consumer.read(&mut [0; MAX_PACKET_BYTES]).expect("request"), BufferRead::Empty);
    assert!(control.take_request());
}

#[test]
fn consumer_activity_follows_track_and_pause_state() {
    let (mut producer, consumer) = frame_buffer(FrameBufferConfig::default());
    assert!(!consumer.is_playing());

    producer.begin_track();
    assert!(consumer.is_playing());
    producer.set_paused(true);
    assert!(!consumer.is_playing());
    producer.set_paused(false);
    assert!(consumer.is_playing());
}
