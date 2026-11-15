use super::*;

#[test]
fn header_is_v2_big_endian() {
    let mut buf = [0u8; RTP_HEADER_LEN];
    write_header(&mut buf, 0x1234, 0xdead_beef, 0xffff_0001, 120, false);
    assert_eq!(buf[0], 0x80);
    assert_eq!(buf[1], 120);
    assert_eq!(&buf[2..4], &[0x12, 0x34]);
    assert_eq!(&buf[4..8], &[0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(&buf[8..12], &[0xff, 0xff, 0x00, 0x01]);
}

#[test]
fn extension_sets_x_bit() {
    let mut buf = [0u8; RTP_HEADER_LEN];
    write_header(&mut buf, 1, 960, 2, 120, true);
    assert_eq!(buf[0], 0x90);
}

#[test]
fn payload_type_masked_to_seven_bits() {
    let mut buf = [0u8; RTP_HEADER_LEN];
    write_header(&mut buf, 0, 0, 0, 0xff, false);
    assert_eq!(buf[1], 0x7f);
}

#[test]
fn counters_advance_and_wrap() {
    let mut rtp = Rtp { ssrc: 7, seq: u16::MAX - 1, timestamp: u32::MAX - 500 };
    let mut buf = [0u8; RTP_HEADER_LEN];

    rtp.next_header(&mut buf, false);
    assert_eq!(u16::from_be_bytes(buf[2..4].try_into().unwrap()), u16::MAX - 1);
    rtp.next_header(&mut buf, false);
    assert_eq!(u16::from_be_bytes(buf[2..4].try_into().unwrap()), u16::MAX);
    rtp.next_header(&mut buf, false);
    assert_eq!(u16::from_be_bytes(buf[2..4].try_into().unwrap()), 0, "seq wraps");
    assert_eq!(
        rtp.timestamp,
        (u32::MAX - 500).wrapping_add(2 * TIMESTAMP_INCREMENT).wrapping_add(TIMESTAMP_INCREMENT),
        "timestamp advances one frame per header"
    );
    assert_eq!(
        u32::from_be_bytes(buf[4..8].try_into().unwrap()),
        rtp.timestamp - TIMESTAMP_INCREMENT
    );
}

#[test]
fn ssrc_and_start_values() {
    let rtp = Rtp::new(0xffff_ffff);
    assert_eq!(rtp.ssrc, u32::MAX);
}

#[test]
fn a_new_ssrc_keeps_audio_time() {
    let mut rtp = Rtp { ssrc: 7, seq: 100, timestamp: 2 * TIMESTAMP_INCREMENT };
    rtp.rebind(42);
    let mut buf = [0u8; RTP_HEADER_LEN];
    rtp.next_header(&mut buf, false);
    assert_eq!(
        u32::from_be_bytes(buf[8..12].try_into().unwrap()),
        42,
        "the header names the ssrc READY gave us now"
    );
    assert_eq!(
        u32::from_be_bytes(buf[4..8].try_into().unwrap()),
        2 * TIMESTAMP_INCREMENT,
        "a fresh transport does not restart the track timeline"
    );
}
