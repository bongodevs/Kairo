use super::*;

#[test]
fn silence_is_one_stereo_twenty_millisecond_frame() {
    let info = inspect_packet(&[0xfc, 0xff, 0xfe]).expect("silence packet");
    assert_eq!(
        info,
        OpusPacketInfo { frame_count: 1, samples_per_frame: 960, total_samples: 960, channels: 2 }
    );
    assert!(info.is_voice_passthrough_frame());
    assert!(inspect_packet(&[0xfc]).expect("DTX packet").is_voice_passthrough_frame());
}

#[test]
fn two_ten_millisecond_frames_are_also_one_voice_tick() {
    let info = inspect_packet(&[0x65, 1, 2]).expect("two CBR frames");
    assert_eq!(info.frame_count, 2);
    assert_eq!(info.samples_per_frame, 480);
    assert_eq!(info.total_samples, 960);
}

#[test]
fn code_two_and_code_three_layouts_are_fully_validated() {
    for packet in
        [&[0x66, 1, 0xaa, 0xbb][..], &[0x67, 0x02, 0xaa, 0xbb], &[0x67, 0xc2, 1, 1, 0xaa, 0xbb, 0]]
    {
        let info = inspect_packet(packet).expect("valid multi-frame packet");
        assert_eq!(info.frame_count, 2);
        assert_eq!(info.total_samples, 960);
    }
}

#[test]
fn malformed_lengths_padding_and_duration_are_rejected() {
    assert_eq!(inspect_packet(&[]), Err(PacketError::Empty));
    assert_eq!(inspect_packet(&[0xfd, 1]), Err(PacketError::FrameLength));
    assert_eq!(inspect_packet(&[0xff, 0]), Err(PacketError::FrameCount));
    assert_eq!(inspect_packet(&[0xff, 0x41]), Err(PacketError::Padding));
    assert_eq!(inspect_packet(&[0x1b, 0x03, 1, 2, 3]), Err(PacketError::Duration));
}

#[test]
fn source_bound_is_larger_than_the_voice_passthrough_bound() {
    let mut packet = vec![0_u8; crate::MAX_SOURCE_PACKET_BYTES];
    packet[0] = 0xf7;
    packet[1] = 2;
    let info = inspect_packet(&packet).expect("large two-frame source packet");
    assert!(info.is_voice_passthrough_frame());
    assert!(!info.can_passthrough_voice(packet.len()));

    packet.push(0);
    assert_eq!(inspect_packet(&packet), Err(PacketError::Oversized));
}
