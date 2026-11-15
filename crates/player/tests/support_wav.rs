pub fn pcm_wave(sample_rate: u32, channels: u16, frames: u32) -> Vec<u8> {
    let bytes_per_sample = 2u16;
    let block_align = channels.checked_mul(bytes_per_sample).expect("block alignment");
    let data_len = frames.checked_mul(u32::from(block_align)).expect("fixture data length");
    let riff_len = 36u32.checked_add(data_len).expect("fixture RIFF length");
    let byte_rate = sample_rate.checked_mul(u32::from(block_align)).expect("fixture byte rate");
    let capacity = usize::try_from(44u32 + data_len).expect("fixture capacity");
    let mut bytes = Vec::with_capacity(capacity);

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());

    for _ in 0..frames {
        for channel in 0..channels {
            let sample = if channel % 2 == 0 { 16_384i16 } else { -16_384i16 };
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    bytes
}
