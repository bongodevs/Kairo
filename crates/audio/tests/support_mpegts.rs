#![allow(dead_code)]

const PACKET_BYTES: usize = 188;
const PROGRAM_MAP_PID: u16 = 0x100;
const AUDIO_PID: u16 = 0x101;

pub fn adts_stream() -> Vec<u8> {
    include_bytes!("fixtures/aac-tone.aac").to_vec()
}

pub fn mpeg_ts_adts() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&packet(0, true, &pat()));
    bytes.extend_from_slice(&packet(0x11, true, &sdt()));
    bytes.extend_from_slice(&packet(PROGRAM_MAP_PID, true, &pmt()));

    let audio = adts_stream();
    let packet_length = u16::try_from(audio.len() + 3).expect("PES length");
    let mut pes = vec![0, 0, 1, 0xc0, (packet_length >> 8) as u8, packet_length as u8, 0x80, 0, 0];
    pes.extend_from_slice(&audio);
    for (index, chunk) in pes.chunks(PACKET_BYTES - 4).enumerate() {
        bytes.extend_from_slice(&packet(AUDIO_PID, index == 0, chunk));
    }
    bytes
}

pub const fn service_author() -> &'static str {
    "Provider"
}

pub const fn service_title() -> &'static str {
    "Station"
}

fn pat() -> Vec<u8> {
    vec![
        0,
        0x00,
        0xb0,
        0x0d,
        0,
        1,
        0xc1,
        0,
        0,
        0,
        1,
        0xe0 | ((PROGRAM_MAP_PID >> 8) as u8 & 0x1f),
        PROGRAM_MAP_PID as u8,
        0,
        0,
        0,
        0,
    ]
}

fn pmt() -> Vec<u8> {
    vec![
        0,
        0x02,
        0xb0,
        0x12,
        0,
        1,
        0xc1,
        0,
        0,
        0xe0 | ((AUDIO_PID >> 8) as u8 & 0x1f),
        AUDIO_PID as u8,
        0xf0,
        0,
        0x0f,
        0xe0 | ((AUDIO_PID >> 8) as u8 & 0x1f),
        AUDIO_PID as u8,
        0xf0,
        0,
        0,
        0,
        0,
        0,
    ]
}

fn sdt() -> Vec<u8> {
    let provider = service_author().as_bytes();
    let title = service_title().as_bytes();
    let descriptor_length = 1 + 1 + provider.len() + 1 + title.len();
    let descriptors_length = descriptor_length + 2;
    let section_length = 8 + 5 + descriptors_length + 4;
    let mut payload = vec![
        0,
        0x42,
        0xb0 | ((section_length >> 8) as u8 & 0x0f),
        section_length as u8,
        0,
        1,
        0xc1,
        0,
        0,
        0,
        1,
        0xff,
        0,
        1,
        0xfc,
        0x80 | ((descriptors_length >> 8) as u8 & 0x0f),
        descriptors_length as u8,
        0x48,
        descriptor_length as u8,
        1,
        provider.len() as u8,
    ];
    payload.extend_from_slice(provider);
    payload.push(title.len() as u8);
    payload.extend_from_slice(title);
    payload.extend_from_slice(&[0, 0, 0, 0]);
    payload
}

fn packet(pid: u16, starts_unit: bool, payload: &[u8]) -> [u8; PACKET_BYTES] {
    assert!(payload.len() <= PACKET_BYTES - 4);
    let mut packet = [0xff; PACKET_BYTES];
    packet[0] = 0x47;
    packet[1] = ((pid >> 8) as u8 & 0x1f) | if starts_unit { 0x40 } else { 0 };
    packet[2] = pid as u8;
    packet[3] = 0x10;
    packet[4..4 + payload.len()].copy_from_slice(payload);
    packet
}
