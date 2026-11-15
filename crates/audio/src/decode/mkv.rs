use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};

use symphonia::core::units::TimeBase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EndTrim {
    pub(super) expected_pts: i64,
    pub(super) frames: usize,
}

pub(super) struct Padding {
    number: u64,
    codec_delay_nanos: u64,
    tail: Option<Tail>,
}

struct Tail {
    timestamp: i64,
    padding_nanos: u64,
}

impl Padding {
    pub(super) fn priming_frames(&self, sample_rate: u32) -> usize {
        usize::try_from(frames_from_nanos(self.codec_delay_nanos, sample_rate))
            .unwrap_or(usize::MAX)
    }

    pub(super) fn end_trim(&self, sample_rate: u32, time_base: TimeBase) -> Option<EndTrim> {
        let tail = self.tail.as_ref()?;
        let frames = usize::try_from(frames_from_nanos(tail.padding_nanos, sample_rate)).ok()?;
        if frames == 0 {
            return None;
        }
        let delay = delay_ticks(self.codec_delay_nanos, time_base)?;
        Some(EndTrim { expected_pts: tail.timestamp.checked_sub(delay)?, frames })
    }
}

pub(super) struct Tracks {
    tracks: Vec<Padding>,
}

impl Tracks {
    pub(super) fn track(&self, number: u64) -> Option<&Padding> {
        self.tracks.iter().find(|track| track.number == number)
    }
}

pub(super) fn read<R: Read + Seek>(input: &mut R) -> Option<Tracks> {
    let tracks = scan(input);
    let _ = input.seek(SeekFrom::Start(0));
    tracks
}

fn scan<R: Read + Seek>(input: &mut R) -> Option<Tracks> {
    let file_end = input.seek(SeekFrom::End(0)).ok()?;
    let (segment_data, segment_end) = segment_bounds(input, file_end)?;

    let mut seek_head = None;
    let mut tracks_header = None;
    let mut cues_header = None;
    let mut position = segment_data;
    for _ in 0..LEADING_ELEMENTS {
        let header = header_at(input, position)?;
        match header.id {
            ID_SEEK_HEAD => seek_head = Some(header),
            ID_TRACKS => tracks_header = Some(header),
            ID_CUES => cues_header = Some(header),
            ID_CLUSTER => break,
            _ => {}
        }
        position = header.end?;
        if position >= segment_end {
            break;
        }
    }
    if let Some(head) = seek_head
        && let Some(seek_body) = body(input, &head)
    {
        let (tracks, cues) = seek_entries(&seek_body);
        tracks_header = tracks_header.or_else(|| located(input, segment_data, ID_TRACKS, tracks?));
        cues_header = cues_header.or_else(|| located(input, segment_data, ID_CUES, cues?));
    }

    let tracks_body = body(input, &tracks_header?)?;
    let mut tracks = audio_tracks(&tracks_body);
    if tracks.is_empty() {
        return None;
    }

    let cues = cues_header
        .and_then(|header| body(input, &header))
        .map(|body| last_cues(&body, &tracks))
        .unwrap_or_default();
    tail_padding(input, segment_data, segment_end, &cues, &mut tracks);

    Some(Tracks {
        tracks: tracks
            .into_iter()
            .map(|track| Padding {
                number: track.number,
                codec_delay_nanos: track.codec_delay_nanos,
                tail: track.tail,
            })
            .collect(),
    })
}

fn located<R: Read + Seek>(
    input: &mut R,
    segment_data: u64,
    id: u64,
    relative: u64,
) -> Option<Header> {
    let header = header_at(input, segment_data.checked_add(relative)?)?;
    (header.id == id).then_some(header)
}

fn segment_bounds<R: Read + Seek>(input: &mut R, file_end: u64) -> Option<(u64, u64)> {
    let mut position = 0u64;
    for _ in 0..LEADING_ELEMENTS {
        let header = header_at(input, position)?;
        if header.id == ID_SEGMENT {
            let end = match header.size() {
                Some(size) => header.data.checked_add(size)?.min(file_end),
                None => file_end,
            };
            return Some((header.data, end));
        }
        position = header.end?;
        if position >= file_end {
            return None;
        }
    }
    None
}

fn seek_entries(body: &[u8]) -> (Option<u64>, Option<u64>) {
    let mut tracks = None;
    let mut cues = None;
    for seek in Elements::new(body).filter(|element| element.id == ID_SEEK) {
        let mut id = None;
        let mut position = None;
        for child in Elements::new(seek.body) {
            match child.id {
                ID_SEEK_ID => id = read_uint(child.body),
                ID_SEEK_POSITION => position = read_uint(child.body),
                _ => {}
            }
        }
        match (id, position) {
            (Some(ID_TRACKS), Some(position)) => tracks = tracks.or(Some(position)),
            (Some(ID_CUES), Some(position)) => cues = cues.or(Some(position)),
            _ => {}
        }
    }
    (tracks, cues)
}

struct AudioTrack {
    number: u64,
    codec_delay_nanos: u64,
    timestamp_scale: f64,
    tail: Option<Tail>,
    resolved: bool,
}

fn audio_tracks(body: &[u8]) -> Vec<AudioTrack> {
    let mut tracks = Vec::new();
    for entry in Elements::new(body).filter(|element| element.id == ID_TRACK_ENTRY) {
        let mut number = None;
        let mut kind = None;
        let mut codec_delay_nanos = 0;
        let mut timestamp_scale = 1.0;
        for child in Elements::new(entry.body) {
            match child.id {
                ID_TRACK_NUMBER => number = read_uint(child.body),
                ID_TRACK_TYPE => kind = read_uint(child.body),
                ID_CODEC_DELAY => codec_delay_nanos = read_uint(child.body).unwrap_or(0),
                ID_TRACK_TIMESTAMP_SCALE => {
                    timestamp_scale = read_float(child.body).unwrap_or(1.0);
                }
                _ => {}
            }
        }
        let Some(number) = number else {
            continue;
        };
        if kind != Some(TRACK_TYPE_AUDIO) || !timestamp_scale.is_finite() || timestamp_scale <= 0.0
        {
            continue;
        }
        tracks.push(AudioTrack {
            number,
            codec_delay_nanos,
            timestamp_scale,
            tail: None,
            resolved: false,
        });
    }
    tracks
}

fn last_cues(body: &[u8], tracks: &[AudioTrack]) -> Vec<(u64, u64, Option<u64>)> {
    let mut cues: Vec<(u64, u64, Option<u64>)> = Vec::new();
    for point in Elements::new(body).filter(|element| element.id == ID_CUE_POINT) {
        for positions in
            Elements::new(point.body).filter(|element| element.id == ID_CUE_TRACK_POSITIONS)
        {
            let mut track = None;
            let mut cluster = None;
            let mut relative = None;
            for child in Elements::new(positions.body) {
                match child.id {
                    ID_CUE_TRACK => track = read_uint(child.body),
                    ID_CUE_CLUSTER_POSITION => cluster = read_uint(child.body),
                    ID_CUE_RELATIVE_POSITION => relative = read_uint(child.body),
                    _ => {}
                }
            }
            let (Some(track), Some(cluster)) = (track, cluster) else {
                continue;
            };
            if !tracks.iter().any(|audio| audio.number == track) {
                continue;
            }
            if let Some(existing) = cues.iter_mut().find(|cue| cue.0 == track) {
                *existing = (track, cluster, relative);
            } else {
                cues.push((track, cluster, relative));
            }
        }
    }
    cues
}

fn tail_padding<R: Read + Seek>(
    input: &mut R,
    segment_data: u64,
    segment_end: u64,
    cues: &[(u64, u64, Option<u64>)],
    tracks: &mut [AudioTrack],
) {
    let start = cues
        .iter()
        .filter_map(|(_, cluster, _)| segment_data.checked_add(*cluster))
        .min()
        .unwrap_or(segment_data)
        .max(segment_data);

    let mut clusters: VecDeque<(u64, u64)> = VecDeque::new();
    let mut position = start;
    let mut walked = 0usize;
    while position < segment_end {
        walked += 1;
        if walked > MAX_TOP_LEVEL {
            return;
        }
        let Some(header) = header_at(input, position) else {
            return;
        };
        let Some(end) = header.end else {
            return;
        };
        if end > segment_end {
            return;
        }
        if header.id == ID_CLUSTER {
            if clusters.len() == TAIL_CLUSTERS {
                clusters.pop_front();
            }
            clusters.push_back((header.data, end - header.data));
        }
        position = end;
    }
    if position != segment_end {
        return;
    }

    while let Some((offset, size)) = clusters.pop_back() {
        if size > MAX_CLUSTER_BYTES {
            continue;
        }
        let Some(body) = read_range(input, offset, size) else {
            return;
        };
        let Some((cluster_timestamp, blocks)) = cluster_blocks(&body) else {
            continue;
        };
        for track in tracks.iter_mut().filter(|track| !track.resolved) {
            let Some(block) = blocks.iter().find(|block| block.track == track.number) else {
                continue;
            };
            track.resolved = true;
            let Some(padding) = block.padding.filter(|padding| *padding > 0) else {
                continue;
            };
            let scaled = (cluster_timestamp as f64 * track.timestamp_scale).round();
            let Some(cluster_timestamp) = float_to_i64(scaled) else {
                continue;
            };
            let Some(timestamp) = cluster_timestamp.checked_add(i64::from(block.relative)) else {
                continue;
            };
            track.tail =
                Some(Tail { timestamp, padding_nanos: u64::try_from(padding).unwrap_or(0) });
        }
        if tracks.iter().all(|track| track.resolved) {
            return;
        }
    }
}

struct Block {
    track: u64,
    relative: i16,
    padding: Option<i64>,
}

fn cluster_blocks(body: &[u8]) -> Option<(u64, Vec<Block>)> {
    let mut cluster_timestamp = None;
    let mut blocks: Vec<Block> = Vec::new();
    for element in Elements::new(body) {
        match element.id {
            ID_TIMESTAMP => cluster_timestamp = read_uint(element.body),
            ID_SIMPLE_BLOCK => {
                let Some(block) = parse_block(element.body) else {
                    continue;
                };
                record(&mut blocks, block.0, block.1, None);
            }
            ID_BLOCK_GROUP => {
                let mut block = None;
                let mut padding = None;
                for child in Elements::new(element.body) {
                    match child.id {
                        ID_BLOCK => block = parse_block(child.body),
                        ID_DISCARD_PADDING => padding = read_int(child.body),
                        _ => {}
                    }
                }
                if let Some((track, relative)) = block {
                    record(&mut blocks, track, relative, padding);
                }
            }
            _ => {}
        }
    }
    cluster_timestamp.map(|timestamp| (timestamp, blocks))
}

fn record(blocks: &mut Vec<Block>, track: u64, relative: i16, padding: Option<i64>) {
    if let Some(existing) = blocks.iter_mut().find(|block| block.track == track) {
        *existing = Block { track, relative, padding };
    } else {
        blocks.push(Block { track, relative, padding });
    }
}

fn parse_block(body: &[u8]) -> Option<(u64, i16)> {
    let mut position = 0usize;
    let track = buffer_vint(body, &mut position, false)?;
    let relative = i16::from_be_bytes(body.get(position..position + 2)?.try_into().ok()?);
    position += 2;
    body.get(position)?;
    Some((track, relative))
}

#[derive(Clone, Copy)]
struct Header {
    id: u64,
    data: u64,
    end: Option<u64>,
}

impl Header {
    fn size(&self) -> Option<u64> {
        self.end?.checked_sub(self.data)
    }
}

fn header_at<R: Read + Seek>(input: &mut R, offset: u64) -> Option<Header> {
    input.seek(SeekFrom::Start(offset)).ok()?;
    let (id, _) = stream_vint(input, true)?;
    let (size, length) = stream_vint(input, false)?;
    let data = input.stream_position().ok()?;
    let size = (!unknown_size(size, length)).then_some(size);
    Some(Header { id, data, end: size.map(|size| data.saturating_add(size)) })
}

fn body<R: Read + Seek>(input: &mut R, header: &Header) -> Option<Vec<u8>> {
    read_range(input, header.data, header.size()?)
}

fn read_range<R: Read + Seek>(input: &mut R, offset: u64, size: u64) -> Option<Vec<u8>> {
    if size > MAX_ELEMENT_BYTES {
        return None;
    }
    input.seek(SeekFrom::Start(offset)).ok()?;
    let mut data = vec![0; usize::try_from(size).ok()?];
    input.read_exact(&mut data).ok()?;
    Some(data)
}

fn stream_vint<R: Read>(input: &mut R, keep_marker: bool) -> Option<(u64, usize)> {
    let mut first = [0u8; 1];
    input.read_exact(&mut first).ok()?;
    let length = vint_length(first[0])?;
    let mut value = if keep_marker {
        u64::from(first[0])
    } else {
        u64::from(first[0] & ((0xffu16 >> length) as u8))
    };
    let mut rest = [0u8; 8];
    let rest = rest.get_mut(..length - 1)?;
    input.read_exact(rest).ok()?;
    for byte in rest {
        value = (value << 8) | u64::from(*byte);
    }
    Some((value, length))
}

fn buffer_vint(data: &[u8], position: &mut usize, keep_marker: bool) -> Option<u64> {
    let start = *position;
    let first = *data.get(start)?;
    let length = vint_length(first)?;
    let mut value =
        if keep_marker { u64::from(first) } else { u64::from(first & ((0xffu16 >> length) as u8)) };
    for index in 1..length {
        value = (value << 8) | u64::from(*data.get(start + index)?);
    }
    *position = start + length;
    Some(value)
}

fn vint_length(first: u8) -> Option<usize> {
    let length = first.leading_zeros() as usize + 1;
    (length <= 8).then_some(length)
}

fn unknown_size(value: u64, length: usize) -> bool {
    value == (1u64 << (7 * length)) - 1
}

fn read_uint(data: &[u8]) -> Option<u64> {
    if data.is_empty() || data.len() > 8 {
        return None;
    }
    Some(data.iter().fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
}

fn read_int(data: &[u8]) -> Option<i64> {
    if data.is_empty() || data.len() > 8 {
        return None;
    }
    let mut value = if data[0] & 0x80 != 0 { -1i64 } else { 0 };
    for byte in data {
        value = (value << 8) | i64::from(*byte);
    }
    Some(value)
}

fn read_float(data: &[u8]) -> Option<f64> {
    match data.len() {
        4 => Some(f64::from(f32::from_be_bytes(data.try_into().ok()?))),
        8 => Some(f64::from_be_bytes(data.try_into().ok()?)),
        _ => None,
    }
}

fn float_to_i64(value: f64) -> Option<i64> {
    if !value.is_finite() || value < i64::MIN as f64 || value > i64::MAX as f64 {
        return None;
    }
    Some(value as i64)
}

fn frames_from_nanos(nanos: u64, sample_rate: u32) -> u64 {
    let scaled = u128::from(nanos) * u128::from(sample_rate);
    u64::try_from((scaled + 500_000_000) / 1_000_000_000).unwrap_or(u64::MAX)
}

fn delay_ticks(nanos: u64, time_base: TimeBase) -> Option<i64> {
    let factor =
        (1_000_000_000u128 * u128::from(time_base.numer.get())) / u128::from(time_base.denom.get());
    if factor == 0 {
        return None;
    }
    i64::try_from(u128::from(nanos) / factor).ok()
}

struct Elements<'a> {
    data: &'a [u8],
    position: usize,
}

struct Element<'a> {
    id: u64,
    body: &'a [u8],
}

impl<'a> Elements<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }
}

impl<'a> Iterator for Elements<'a> {
    type Item = Element<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut position = self.position;
        let id = buffer_vint(self.data, &mut position, true)?;
        let size_start = position;
        let size = buffer_vint(self.data, &mut position, false)?;
        let length = vint_length(*self.data.get(size_start)?)?;
        if unknown_size(size, length) {
            return None;
        }
        let size = usize::try_from(size).ok()?;
        let body = self.data.get(position..position.checked_add(size)?)?;
        self.position = position + size;
        Some(Element { id, body })
    }
}

const ID_SEGMENT: u64 = 0x1853_8067;
const ID_SEEK_HEAD: u64 = 0x114d_9b74;
const ID_SEEK: u64 = 0x4dbb;
const ID_SEEK_ID: u64 = 0x53ab;
const ID_SEEK_POSITION: u64 = 0x53ac;
const ID_TRACKS: u64 = 0x1654_ae6b;
const ID_TRACK_ENTRY: u64 = 0xae;
const ID_TRACK_NUMBER: u64 = 0xd7;
const ID_TRACK_TYPE: u64 = 0x83;
const ID_CODEC_DELAY: u64 = 0x56aa;
const ID_TRACK_TIMESTAMP_SCALE: u64 = 0x0023_314f;
const ID_CLUSTER: u64 = 0x1f43_b675;
const ID_TIMESTAMP: u64 = 0xe7;
const ID_SIMPLE_BLOCK: u64 = 0xa3;
const ID_BLOCK_GROUP: u64 = 0xa0;
const ID_BLOCK: u64 = 0xa1;
const ID_DISCARD_PADDING: u64 = 0x75a2;
const ID_CUES: u64 = 0x1c53_bb6b;
const ID_CUE_POINT: u64 = 0xbb;
const ID_CUE_TRACK_POSITIONS: u64 = 0xb7;
const ID_CUE_TRACK: u64 = 0xf7;
const ID_CUE_CLUSTER_POSITION: u64 = 0xf1;
const ID_CUE_RELATIVE_POSITION: u64 = 0xf0;

const TRACK_TYPE_AUDIO: u64 = 2;
const LEADING_ELEMENTS: usize = 16;
const MAX_TOP_LEVEL: usize = 16_384;
const TAIL_CLUSTERS: usize = 64;
const MAX_ELEMENT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CLUSTER_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
#[path = "../../tests/decode_mkv.rs"]
mod tests;
