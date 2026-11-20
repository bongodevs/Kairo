use super::*;
use crate::audio::{FRAME_SAMPLES, MAX_PACKET_BYTES};
use crate::decode::Millis;
fn frame(value: u8, position: u64) -> BufferedFrame {
    BufferedFrame::new(&[value; 32], Millis::new(position), FRAME_SAMPLES).expect("frame")
}

#[test]
fn duration_is_bounded_and_maps_to_twenty_ms_slots() {
    let config = FrameBufferConfig::default();
    assert_eq!(config.duration_ms(), 5_000);
    assert_eq!(config.capacity(), 251);
    assert_eq!(config.maximum_packet_bytes(), 251 * MAX_PACKET_BYTES);
    assert!(matches!(FrameBufferConfig::new(199), Err(BufferError::InvalidDuration { .. })));
    assert!(matches!(FrameBufferConfig::new(5_001), Err(BufferError::InvalidDuration { .. })));
}

#[tokio::test]
async fn frames_are_fifo_and_reads_never_wait() {
    let config = FrameBufferConfig::new(200).expect("config");
    let (mut producer, mut consumer) = frame_buffer(config);
    assert_eq!(consumer.read(&mut [0; 32]).expect("empty"), BufferRead::Empty);

    producer.send(frame(1, 10)).await.expect("first");
    producer.send(frame(2, 30)).await.expect("second");
    assert!(producer.has_received_frames());
    assert_eq!(producer.last_input_position(), Some(Millis::new(30)));
    let mut output = [0u8; MAX_PACKET_BYTES];
    let first = consumer.read(&mut output).expect("first read");
    assert_eq!(output[..32], [1; 32]);
    assert_eq!(
        first,
        BufferRead::Frame(FrameMeta {
            position: Millis::new(10),
            pcm_frames: 960,
            packet_length: 32,
        })
    );
    assert_eq!(producer.last_input_position(), Some(Millis::new(30)));
    let BufferRead::Frame(second) = consumer.read(&mut output).expect("second read") else {
        panic!("expected second frame");
    };
    assert_eq!(second.position, Millis::new(30));
    assert!(consumer.is_empty());
}

#[tokio::test]
async fn slow_decode_progress_does_not_emit_stuck_but_a_real_stall_does_once() {
    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    let control = producer.control();
    control.set_stuck_threshold(20);
    producer.begin_track();
    let mut output = [0u8; MAX_PACKET_BYTES];

    assert_eq!(consumer.read(&mut output).expect("buffering"), BufferRead::Empty);
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(consumer.read(&mut output).expect("still buffering"), BufferRead::Empty);
    assert_eq!(control.stuck_sequence(), 0);

    producer.send(frame(1, 0)).await.expect("first frame");
    assert!(matches!(consumer.read(&mut output).expect("frame"), BufferRead::Frame(_)));
    for _ in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        producer.note_worker_progress();
        assert_eq!(consumer.read(&mut output).expect("slow progress"), BufferRead::Empty);
    }
    assert_eq!(control.stuck_sequence(), 0);

    assert_eq!(consumer.read(&mut output).expect("start stall"), BufferRead::Empty);
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(consumer.read(&mut output).expect("stuck"), BufferRead::Empty);
    assert_eq!(control.stuck_sequence(), 1);
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(consumer.read(&mut output).expect("still stuck"), BufferRead::Empty);
    assert_eq!(control.stuck_sequence(), 1);

    producer.clear_on_insert();
    producer.send(frame(2, 20)).await.expect("post-seek frame");
    assert!(matches!(consumer.read(&mut output).expect("post-seek frame"), BufferRead::Frame(_)));
    assert_eq!(consumer.read(&mut output).expect("second stall"), BufferRead::Empty);
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(consumer.read(&mut output).expect("same track"), BufferRead::Empty);
    assert_eq!(control.stuck_sequence(), 1);

    producer.begin_track();
    producer.send(frame(3, 0)).await.expect("next track frame");
    assert!(matches!(consumer.read(&mut output).expect("next track frame"), BufferRead::Frame(_)));
    assert_eq!(consumer.read(&mut output).expect("next stall"), BufferRead::Empty);
    std::thread::sleep(std::time::Duration::from_millis(25));
    assert_eq!(consumer.read(&mut output).expect("next stuck"), BufferRead::Empty);
    assert_eq!(control.stuck_sequence(), 2);
}

#[tokio::test]
async fn a_small_output_does_not_lose_the_frame() {
    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    producer.send(frame(7, 0)).await.expect("send");
    let error = consumer.read(&mut [0; 4]).expect_err("too small");
    assert!(matches!(error, BufferError::OutputTooSmall { required: 32, .. }));

    let mut output = [0u8; MAX_PACKET_BYTES];
    assert!(matches!(consumer.read(&mut output).expect("retry"), BufferRead::Frame(_)));
    assert_eq!(output[..32], [7; 32]);
}

#[tokio::test]
async fn maximum_packets_use_fixed_storage_without_overflow_allocation() {
    let packet = vec![0x3c; 600];
    let frame = BufferedFrame::new(&packet, Millis::default(), FRAME_SAMPLES).expect("large frame");
    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    producer.send(frame).await.expect("send");
    let mut output = [0u8; MAX_PACKET_BYTES];
    assert!(matches!(consumer.read(&mut output).expect("read"), BufferRead::Frame(_)));
    assert_eq!(&output[..packet.len()], packet);
    assert!(matches!(
        BufferedFrame::new(&vec![0; MAX_PACKET_BYTES + 1], Millis::default(), FRAME_SAMPLES),
        Err(BufferError::PacketTooLarge { .. })
    ));
}

#[tokio::test]
async fn a_full_buffer_applies_backpressure_until_voice_consumes() {
    let config = FrameBufferConfig::new(200).expect("config");
    let (mut producer, mut consumer) = frame_buffer(config);
    for index in 0..config.capacity() {
        producer
            .send(frame(
                u8::try_from(index).expect("small capacity"),
                u64::try_from(index).expect("small capacity") * 20,
            ))
            .await
            .expect("fill");
    }
    assert_eq!(producer.remaining_capacity(), 0);

    let blocked = tokio::spawn(async move {
        producer.send(frame(99, 999)).await?;
        Ok::<_, BufferError>(producer)
    });
    tokio::task::yield_now().await;
    assert!(!blocked.is_finished(), "the producer must wait, never drop");

    let mut output = [0u8; MAX_PACKET_BYTES];
    assert!(matches!(consumer.read(&mut output).expect("consume"), BufferRead::Frame(_)));
    let producer = blocked.await.expect("producer task").expect("send resumes");
    assert_eq!(producer.remaining_capacity(), 0);
}

#[tokio::test]
async fn clear_on_insert_keeps_old_audio_until_replacement_is_ready() {
    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    producer.send(frame(1, 0)).await.expect("old one");
    producer.send(frame(2, 20)).await.expect("old two");
    producer.clear_on_insert();
    producer.send(frame(9, 1_000)).await.expect("replacement");

    let mut output = [0u8; MAX_PACKET_BYTES];
    let read = consumer.read(&mut output).expect("read replacement");
    let BufferRead::Frame(meta) = read else {
        panic!("expected replacement frame");
    };
    assert_eq!(meta.position, Millis::new(1_000));
    assert_eq!(output[..32], [9; 32]);
    assert!(consumer.is_empty());
}

#[tokio::test]
async fn clear_on_insert_uses_end_as_a_replacement_when_seek_reaches_eof() {
    let (mut producer, mut consumer) = frame_buffer(FrameBufferConfig::default());
    producer.send(frame(1, 0)).await.expect("old one");
    producer.send(frame(2, 20)).await.expect("old two");
    producer.clear_on_insert();
    producer.send_end().await.expect("replacement end");

    assert_eq!(consumer.read(&mut [0; MAX_PACKET_BYTES]).expect("end"), BufferRead::End);
    assert!(consumer.is_empty());
}

#[tokio::test]
async fn finish_is_ordered_after_audio_and_reader_close_wakes_writers() {
    let config = FrameBufferConfig::new(200).expect("config");
    let (mut producer, mut consumer) = frame_buffer(config);
    producer.send(frame(1, 0)).await.expect("frame");
    producer.finish().await.expect("finish");
    let mut output = [0u8; MAX_PACKET_BYTES];
    assert!(matches!(consumer.read(&mut output).expect("frame read"), BufferRead::Frame(_)));
    assert_eq!(consumer.read(&mut output).expect("end"), BufferRead::End);
    assert_eq!(consumer.read(&mut output).expect("closed"), BufferRead::Closed);

    let (mut producer, mut consumer) = frame_buffer(config);
    for index in 0..config.capacity() {
        producer.send(frame(u8::try_from(index).expect("small capacity"), 0)).await.expect("fill");
    }
    let blocked = tokio::spawn(async move { producer.send(frame(99, 0)).await });
    tokio::task::yield_now().await;
    consumer.close();
    assert!(matches!(blocked.await.expect("writer task"), Err(BufferError::Closed)));
}

#[tokio::test]
async fn raw_pcm_s16_frame_buffer_roundtrip() {
    use crate::buffer::{FrameFormat, MAX_PCM_S16_BYTES};

    let config = FrameBufferConfig::with_format(200, FrameFormat::PcmS16Le).expect("config");
    assert_eq!(config.format(), FrameFormat::PcmS16Le);
    assert_eq!(config.maximum_packet_bytes(), config.capacity() * MAX_PCM_S16_BYTES);

    let (mut producer, mut consumer) = frame_buffer(config);
    assert_eq!(producer.format(), FrameFormat::PcmS16Le);
    assert_eq!(consumer.format(), FrameFormat::PcmS16Le);

    let samples: Vec<i16> = (0..1_920).map(|i| (i as i16).wrapping_mul(17)).collect();
    let frame =
        BufferedFrame::new_pcm_s16(&samples, Millis::new(40), FRAME_SAMPLES).expect("pcm frame");
    assert_eq!(frame.format(), FrameFormat::PcmS16Le);

    producer.send(frame).await.expect("send");

    let mut output = [0i16; 1_920];
    let read = consumer.read_pcm_s16(&mut output).expect("read");
    let BufferRead::Frame(meta) = read else {
        panic!("expected frame");
    };
    assert_eq!(meta.position.get(), 40);
    assert_eq!(meta.packet_length as usize, MAX_PCM_S16_BYTES);
    assert_eq!(output, samples.as_slice());
}
