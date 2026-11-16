use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};

use player::source::{OpenedSource, SourceHints, SourceMetadata, SourceReference};

use crate::utils::locked;

/// How long the generated stream lasts.
const PLACEHOLDER_MILLIS: u64 = 30_000;
/// One Opus packet and one granule step.
const FRAME_MILLIS: u64 = 20;
const FRAME_SAMPLES: u64 = 960;
/// A valid 20 ms stereo Opus frame that carries no audio.
const SILENCE_PACKET: [u8; 3] = [0xfc, 0xff, 0xfe];
/// One page per second keeps the stream small without a page per frame.
const PACKETS_PER_PAGE: u64 = 50;

/// Open a generated Ogg Opus stream of silence for this track.
pub(super) fn opened_source(
    source_name: &str,
    reference: &SourceReference,
    metadata: &SourceMetadata,
) -> OpenedSource {
    let bytes = silence_stream();
    let length = u64::try_from(bytes.len()).expect("placeholder stream length");
    OpenedSource::seekable(
        source_name,
        reference.clone(),
        SourceHints {
            mime_type: Some(String::from("audio/ogg")),
            file_extension: Some(String::from("ogg")),
        },
        Some(length),
        Cursor::new(bytes),
    )
    .with_metadata(metadata.clone())
}

/// Tracks that opened as generated silence, so the server can report one exception instead of an
/// end when the stream runs out. A count keeps two concurrent players of the same track apart.
#[derive(Default)]
pub(crate) struct PlaceholderLog {
    entries: Mutex<HashMap<(String, String), usize>>,
}

impl PlaceholderLog {
    pub(crate) fn record(&self, source: &str, reference: &SourceReference) {
        *locked(&self.entries)
            .entry((source.to_owned(), reference.identifier().to_owned()))
            .or_default() += 1;
    }

    /// Take one recorded placeholder. `false` means the open produced real media.
    pub(crate) fn take(&self, source: &str, reference: &SourceReference) -> bool {
        let mut entries = locked(&self.entries);
        let key = (source.to_owned(), reference.identifier().to_owned());
        let Some(count) = entries.get_mut(&key) else {
            return false;
        };
        *count -= 1;
        if *count == 0 {
            entries.remove(&key);
        }
        true
    }
}

/// The same immutable stream serves every placeholder open.
fn silence_stream() -> Arc<[u8]> {
    static STREAM: OnceLock<Arc<[u8]>> = OnceLock::new();
    Arc::clone(STREAM.get_or_init(|| build_stream().into()))
}

fn build_stream() -> Vec<u8> {
    let page_count = PLACEHOLDER_MILLIS / FRAME_MILLIS / PACKETS_PER_PAGE;
    let mut stream = Vec::with_capacity(8 * 1024);
    let serial = 0x4b41_4952;
    let mut head = Vec::from(&b"OpusHead"[..]);
    head.extend_from_slice(&[1, 2]);
    head.extend_from_slice(&0u16.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    stream.extend_from_slice(&page(serial, 0, 0x02, 0, &[&head]));

    let mut tags = Vec::from(&b"OpusTags"[..]);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags.extend_from_slice(&0u32.to_le_bytes());
    stream.extend_from_slice(&page(serial, 1, 0, 0, &[&tags]));

    let mut granule = 0u64;
    for index in 0..page_count {
        let packets = vec![
            SILENCE_PACKET.as_slice();
            usize::try_from(PACKETS_PER_PAGE).expect("packets per page")
        ];
        granule += FRAME_SAMPLES * PACKETS_PER_PAGE;
        let last = index + 1 == page_count;
        stream.extend_from_slice(&page(
            serial,
            u32::try_from(index + 2).expect("page sequence"),
            if last { 0x04 } else { 0 },
            granule,
            &packets,
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

#[cfg(test)]
#[path = "../tests/plugins_placeholder.rs"]
mod tests;
