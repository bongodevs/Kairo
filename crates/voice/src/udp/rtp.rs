pub const RTP_HEADER_LEN: usize = 12;
pub const OPUS_PAYLOAD_TYPE: u8 = 120;
pub const TIMESTAMP_INCREMENT: u32 = 960;

pub fn write_header(
    out: &mut [u8; RTP_HEADER_LEN],
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    payload_type: u8,
    extension: bool,
) {
    out[0] = if extension { 0x90 } else { 0x80 };
    out[1] = payload_type & 0x7f;
    out[2..4].copy_from_slice(&seq.to_be_bytes());
    out[4..8].copy_from_slice(&timestamp.to_be_bytes());
    out[8..12].copy_from_slice(&ssrc.to_be_bytes());
}

#[derive(Debug)]
pub struct Rtp {
    ssrc: u32,
    seq: u16,
    timestamp: u32,
}

impl Rtp {
    pub fn new(ssrc: u32) -> Self {
        Self { ssrc, seq: random_sequence(), timestamp: 0 }
    }

    pub(crate) fn rebind(&mut self, ssrc: u32) {
        self.ssrc = ssrc;
        self.seq = random_sequence();
    }

    pub fn next_header(&mut self, out: &mut [u8; RTP_HEADER_LEN], extension: bool) {
        write_header(out, self.seq, self.timestamp, self.ssrc, OPUS_PAYLOAD_TYPE, extension);
        self.seq = self.seq.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(TIMESTAMP_INCREMENT);
    }
}

fn random_sequence() -> u16 {
    getrandom::u32().unwrap_or(0) as u16
}

#[cfg(test)]
#[path = "../../tests/rtp.rs"]
mod tests;
