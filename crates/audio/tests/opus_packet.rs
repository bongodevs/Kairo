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

#[test]
fn mutation_test_opus_packet_inspector_never_panics_or_hangs() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0
        }
        fn next_usize(&mut self, max: usize) -> usize {
            if max == 0 { 0 } else { (self.next() as usize) % max }
        }
    }

    let seed: u64 = 0x1234_5678_9ABC_DEF0;
    let mut rng = Rng(seed);

    let seeds = [
        vec![0xfc, 0xff, 0xfe],                // silence (Code 0)
        vec![0xfc],                            // DTX (Code 0)
        vec![0x65, 1, 2],                      // two CBR frames (Code 1)
        vec![0x66, 1, 0xaa, 0xbb],             // Code 2 (VBR 2 frames)
        vec![0x67, 0x02, 0xaa, 0xbb],          // Code 3 (2 frames, no vbr, no padding)
        vec![0x67, 0xc2, 1, 1, 0xaa, 0xbb, 0], // Code 3 (2 frames, vbr, padding)
    ];

    let total = 100_000;
    let mut first_step_rejected = 0;
    let mut deep_path_reached = 0;

    for iteration in 0..total {
        let (mutated, is_structure_aware) = if rng.next_usize(100) < 75 {
            // Structure-aware mutation: manipulate TOC, frame counts, VBR lengths, padding
            let base = &seeds[rng.next_usize(seeds.len())];
            let mut packet = base.clone();
            match rng.next_usize(5) {
                0 => {
                    // Mutate TOC config bits (keep code bits intact)
                    let config = (rng.next_usize(32) as u8) << 3;
                    let stereo = ((rng.next_usize(2) as u8) & 0x01) << 2;
                    let code = packet[0] & 0x03;
                    packet[0] = config | stereo | code;
                }
                1 => {
                    // If Code 1 (CBR), mutate payload preserving even length
                    if (packet[0] & 0x03) == 1 {
                        let add_pairs = rng.next_usize(4) * 2;
                        for _ in 0..add_pairs {
                            packet.push((rng.next() & 0xFF) as u8);
                        }
                    } else if packet.len() > 1 {
                        let idx = 1 + rng.next_usize(packet.len() - 1);
                        packet[idx] ^= (rng.next() & 0xFF) as u8;
                    }
                }
                2 => {
                    // If Code 2 (VBR), mutate first frame length byte
                    if (packet[0] & 0x03) == 2 && packet.len() > 1 {
                        packet[1] = (rng.next() & 0xFF) as u8;
                    } else if packet.len() > 1 {
                        let idx = 1 + rng.next_usize(packet.len() - 1);
                        packet[idx] = (rng.next() & 0xFF) as u8;
                    }
                }
                3 => {
                    // If Code 3, mutate control byte: frame count 1..48, toggle VBR / padding
                    if (packet[0] & 0x03) == 3 && packet.len() > 1 {
                        let count = (1 + rng.next_usize(48)) as u8;
                        let vbr = if rng.next_usize(2) == 1 { 0x80 } else { 0x00 };
                        let pad = if rng.next_usize(2) == 1 { 0x40 } else { 0x00 };
                        packet[1] = vbr | pad | (count & 0x3F);
                    } else if packet.len() > 1 {
                        let idx = 1 + rng.next_usize(packet.len() - 1);
                        packet[idx] ^= (rng.next() & 0xFF) as u8;
                    }
                }
                4 => {
                    // Swap adjacent bytes in payload
                    if packet.len() > 2 {
                        let idx = 1 + rng.next_usize(packet.len() - 2);
                        packet.swap(idx, idx + 1);
                    }
                }
                _ => unreachable!(),
            }
            (packet, true)
        } else {
            // Raw byte mutation: flip, truncate, extend, splice
            let base = &seeds[rng.next_usize(seeds.len())];
            let mut packet = base.clone();
            match rng.next_usize(4) {
                0 => {
                    let count = 1 + rng.next_usize(4);
                    for _ in 0..count {
                        if !packet.is_empty() {
                            let idx = rng.next_usize(packet.len());
                            packet[idx] ^= (rng.next() & 0xFF) as u8;
                        }
                    }
                }
                1 => {
                    let new_len = rng.next_usize(packet.len() + 1);
                    packet.truncate(new_len);
                }
                2 => {
                    let extra = 1 + rng.next_usize(32);
                    for _ in 0..extra {
                        packet.push((rng.next() & 0xFF) as u8);
                    }
                }
                3 => {
                    if !packet.is_empty() {
                        let start = rng.next_usize(packet.len());
                        let count = rng.next_usize(packet.len() - start);
                        for byte in packet.iter_mut().skip(start).take(count) {
                            *byte = (rng.next() & 0xFF) as u8;
                        }
                    }
                }
                _ => unreachable!(),
            }
            (packet, false)
        };

        // Must never panic or hang
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inspect_packet(&mutated)));

        match result {
            Ok(inspect_result) => match inspect_result {
                Err(PacketError::Empty) | Err(PacketError::Oversized) => {
                    first_step_rejected += 1;
                }
                _ => {
                    deep_path_reached += 1;
                }
            },
            Err(_) => {
                panic!(
                    "inspect_packet panicked at iteration {iteration} with seed {seed:#x}, structure_aware={is_structure_aware}, mutated: {mutated:?}"
                );
            }
        }
    }

    let deep_pct = (deep_path_reached as f64 / total as f64) * 100.0;
    println!(
        "Opus packet: total={total}, first_step_rejected={first_step_rejected}, deep_path_reached={deep_path_reached} ({deep_pct:.2}%)"
    );
    assert!(deep_pct > 20.0, "Expected > 20% deep path reach, got {deep_pct:.2}%");
}
