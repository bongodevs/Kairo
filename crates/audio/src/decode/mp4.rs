use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Gapless {
    pub(super) priming: u64,
    pub(super) total: Option<u64>,
}

pub(super) struct EditList {
    priming: u64,
    media_timescale: u32,
    duration: Option<(u64, u32)>,
}

impl EditList {
    pub(super) fn window(self, sample_rate: u32) -> Option<Gapless> {
        let rate = u128::from(sample_rate);
        let timescale = u128::from(self.media_timescale);
        if timescale == 0 || rate == 0 {
            return None;
        }
        let priming = u64::try_from(u128::from(self.priming) * rate / timescale).ok()?;
        let total = self.duration.and_then(|(ticks, movie_timescale)| {
            let scale = u128::from(movie_timescale.max(1));
            u64::try_from(u128::from(ticks) * rate / scale).ok()
        });
        Some(Gapless { priming, total })
    }
}

pub(super) fn read<R: Read + Seek>(input: &mut R) -> Option<EditList> {
    let edit = scan(input);
    let _ = input.seek(SeekFrom::Start(0));
    edit
}

fn scan<R: Read + Seek>(input: &mut R) -> Option<EditList> {
    let end = input.seek(SeekFrom::End(0)).ok()?;
    let mut offset = 0u64;
    for _ in 0..MAX_BOXES {
        if offset.checked_add(8)? > end {
            return None;
        }
        input.seek(SeekFrom::Start(offset)).ok()?;
        let mut header = [0u8; 8];
        input.read_exact(&mut header).ok()?;
        let size32 = u32::from_be_bytes(header[..4].try_into().ok()?);
        let kind = &header[4..8];
        let mut header_len = 8u64;
        let size = match size32 {
            0 => end - offset,
            1 => {
                let mut large = [0u8; 8];
                input.read_exact(&mut large).ok()?;
                header_len = 16;
                u64::from_be_bytes(large)
            }
            size => u64::from(size),
        };
        if size < header_len || offset.checked_add(size)? > end {
            return None;
        }
        if kind == b"moov" {
            let content = usize::try_from(size - header_len).ok()?;
            if content > MAX_MOOV_BYTES {
                return None;
            }
            let mut data = vec![0u8; content];
            input.read_exact(&mut data).ok()?;
            return parse_moov(&data);
        }
        offset += size;
    }
    None
}

fn parse_moov(data: &[u8]) -> Option<EditList> {
    let movie_timescale = Boxes::new(data)
        .find_map(|(kind, content)| (kind == b"mvhd").then(|| timescale(content)).flatten())
        .unwrap_or(0);
    for (kind, content) in Boxes::new(data) {
        if kind != b"trak" {
            continue;
        }
        let Some((media_timescale, priming, segment)) = audio_edit(content) else {
            continue;
        };
        let duration = (segment > 0 && movie_timescale > 0).then_some((segment, movie_timescale));
        return Some(EditList { priming, media_timescale, duration });
    }
    None
}

fn audio_edit(trak: &[u8]) -> Option<(u32, u64, u64)> {
    let media = child(trak, b"mdia")?;
    let handler = child(media, b"hdlr")?;
    if handler.get(8..12)? != b"soun" {
        return None;
    }
    let media_timescale = timescale(child(media, b"mdhd")?)?;
    let edit = child(child(trak, b"edts")?, b"elst")?;
    let (priming, segment) = elst_entry(edit)?;
    Some((media_timescale, priming, segment))
}

fn elst_entry(data: &[u8]) -> Option<(u64, u64)> {
    let version = *data.first()?;
    let count = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?).min(MAX_EDIT_ENTRIES);
    let mut offset = 8usize;
    for _ in 0..count {
        let (segment, media) = match version {
            1 => {
                let segment = u64::from_be_bytes(data.get(offset..offset + 8)?.try_into().ok()?);
                let media = i64::from_be_bytes(data.get(offset + 8..offset + 16)?.try_into().ok()?);
                offset += 8 + 8 + 4;
                (segment, media)
            }
            _ => {
                let segment =
                    u64::from(u32::from_be_bytes(data.get(offset..offset + 4)?.try_into().ok()?));
                let media = i64::from(i32::from_be_bytes(
                    data.get(offset + 4..offset + 8)?.try_into().ok()?,
                ));
                offset += 4 + 4 + 4;
                (segment, media)
            }
        };
        if media >= 0 {
            return Some((u64::try_from(media).ok()?, segment));
        }
    }
    None
}

fn timescale(data: &[u8]) -> Option<u32> {
    let offset = if data.first() == Some(&1) { 4 + 8 + 8 } else { 4 + 4 + 4 };
    Some(u32::from_be_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

fn child<'a>(data: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    Boxes::new(data).find_map(|(kind, content)| (kind == name).then_some(content))
}

struct Boxes<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Boxes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }
}

impl<'a> Iterator for Boxes<'a> {
    type Item = (&'a [u8; 4], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.data.get(self.position..)?;
        if rest.len() < 8 {
            return None;
        }
        let size32 = u32::from_be_bytes(rest[..4].try_into().ok()?);
        let kind = rest[4..8].try_into().ok()?;
        let mut header = 8usize;
        let size = match size32 {
            0 => rest.len(),
            1 => {
                header = 16;
                usize::try_from(u64::from_be_bytes(rest.get(8..16)?.try_into().ok()?)).ok()?
            }
            size => usize::try_from(size).ok()?,
        };
        if size < header || size > rest.len() {
            return None;
        }
        self.position += size;
        Some((kind, &rest[header..size]))
    }
}

const MAX_BOXES: usize = 4096;
const MAX_MOOV_BYTES: usize = 32 * 1024 * 1024;
const MAX_EDIT_ENTRIES: u32 = 64;

#[cfg(test)]
#[path = "../../tests/decode_mp4.rs"]
mod tests;
