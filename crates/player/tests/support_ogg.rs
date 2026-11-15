pub fn opus_stream(packets: &[&[u8]], channels: u8, pre_skip: u16) -> Vec<u8> {
    let serial = 0x4b41_4952;
    let mut stream = Vec::new();
    let mut head = Vec::from(&b"OpusHead"[..]);
    head.extend_from_slice(&[1, channels]);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    stream.extend_from_slice(&page(serial, 0, 0x02, 0, &[&head]));

    let mut tags = Vec::from(&b"OpusTags"[..]);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags.extend_from_slice(&0u32.to_le_bytes());
    stream.extend_from_slice(&page(serial, 1, 0, 0, &[&tags]));

    let mut granule = 0u64;
    for (index, packet) in packets.iter().enumerate() {
        granule += packet_samples(packet) as u64;
        let last = index + 1 == packets.len();
        stream.extend_from_slice(&page(
            serial,
            u32::try_from(index + 2).expect("page sequence"),
            if last { 0x04 } else { 0 },
            granule,
            &[*packet],
        ));
    }
    stream
}

fn page(serial: u32, sequence: u32, flags: u8, granule: u64, packets: &[&[u8]]) -> Vec<u8> {
    let mut lacing = Vec::new();
    let mut payload = Vec::new();
    for packet in packets {
        let mut left = packet.len();
        while left >= 255 {
            lacing.push(255);
            left -= 255;
        }
        lacing.push(u8::try_from(left).expect("lacing remainder"));
        payload.extend_from_slice(packet);
    }

    let mut page = Vec::with_capacity(27 + lacing.len() + payload.len());
    page.extend_from_slice(b"OggS");
    page.extend_from_slice(&[0, flags]);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());
    page.extend_from_slice(&0u32.to_le_bytes());
    page.push(u8::try_from(lacing.len()).expect("segment count"));
    page.extend_from_slice(&lacing);
    page.extend_from_slice(&payload);
    let checksum = crc(&page);
    page[22..26].copy_from_slice(&checksum.to_le_bytes());
    page
}

fn packet_samples(packet: &[u8]) -> usize {
    let toc = packet[0];
    let config = toc >> 3;
    let per_frame = match config {
        0..=11 => [480, 960, 1_920, 2_880][usize::from(config & 3)],
        12..=15 => [480, 960][usize::from(config & 1)],
        _ => [120, 240, 480, 960][usize::from(config & 3)],
    };
    let count = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => usize::from(packet[1] & 0x3f),
    };
    per_frame * count
}

fn crc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 == 0 { crc << 1 } else { (crc << 1) ^ 0x04c1_1db7 };
        }
    }
    crc
}
