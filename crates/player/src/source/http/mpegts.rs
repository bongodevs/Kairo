use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, ReadBuf};

use kairo_core::SourceMetadataUpdates;

const TS_PACKET_BYTES: usize = 188;
const STREAM_TYPE_ADTS: u8 = 0x0f;

pub(crate) struct TsToAdts<R> {
    input: R,
    packet: [u8; TS_PACKET_BYTES],
    packet_length: usize,
    output: [u8; TS_PACKET_BYTES],
    output_start: usize,
    output_end: usize,
    program_map_pid: Option<u16>,
    audio_pid: Option<u16>,
    pes_bytes_left: Option<usize>,
    metadata: Option<SourceMetadataUpdates>,
    finished: bool,
}

impl<R> TsToAdts<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            input,
            packet: [0; TS_PACKET_BYTES],
            packet_length: 0,
            output: [0; TS_PACKET_BYTES],
            output_start: 0,
            output_end: 0,
            program_map_pid: None,
            audio_pid: None,
            pes_bytes_left: None,
            metadata: None,
            finished: false,
        }
    }

    pub(crate) fn with_metadata(input: R, metadata: SourceMetadataUpdates) -> Self {
        let mut stream = Self::new(input);
        stream.metadata = Some(metadata);
        stream
    }

    fn poll_packet(&mut self, context: &mut Context<'_>) -> Poll<io::Result<bool>>
    where
        R: AsyncRead + Unpin,
    {
        while self.packet_length < TS_PACKET_BYTES {
            let mut output = ReadBuf::new(&mut self.packet[self.packet_length..]);
            ready!(Pin::new(&mut self.input).poll_read(context, &mut output))?;
            let read = output.filled().len();
            if read == 0 {
                if self.packet_length == 0 {
                    self.finished = true;
                    return Poll::Ready(Ok(false));
                }
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "MPEG-TS stream ended within a packet",
                )));
            }
            self.packet_length += read;
        }
        Poll::Ready(Ok(true))
    }

    fn process_packet(&mut self) -> io::Result<()> {
        let packet = &self.packet;
        if packet[0] != 0x47 {
            return Err(io::Error::other("invalid MPEG-TS sync byte"));
        }
        let Some(payload_start) = payload_start(packet)? else {
            self.packet_length = 0;
            return Ok(());
        };
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        let starts_unit = packet[1] & 0x40 != 0;
        let payload = &packet[payload_start..];

        if pid == 0 {
            if let Some(program_map_pid) = parse_pat(payload, starts_unit) {
                self.program_map_pid = Some(program_map_pid);
            }
        } else if Some(pid) == self.program_map_pid {
            if let Some(audio_pid) = parse_pmt(payload, starts_unit) {
                self.audio_pid = Some(audio_pid);
            }
        } else if Some(pid) == self.audio_pid {
            let payload = if starts_unit {
                let Some((payload, remaining)) = pes_payload(payload) else {
                    self.packet_length = 0;
                    return Ok(());
                };
                self.pes_bytes_left = remaining;
                payload
            } else {
                let length = self.pes_bytes_left.unwrap_or(payload.len()).min(payload.len());
                if let Some(remaining) = &mut self.pes_bytes_left {
                    *remaining -= length;
                }
                &payload[..length]
            };
            self.output[..payload.len()].copy_from_slice(payload);
            self.output_start = 0;
            self.output_end = payload.len();
        } else if pid == 0x11
            && let Some((author, title)) = parse_sdt(payload, starts_unit)
            && let Some(metadata) = self.metadata.take()
            && let Ok(mut metadata) = metadata.lock()
        {
            metadata.author = Some(author);
            metadata.title = Some(title);
        }
        self.packet_length = 0;
        Ok(())
    }
}

impl<R> AsyncRead for TsToAdts<R>
where
    R: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let stream = self.get_mut();
        loop {
            if stream.output_start < stream.output_end {
                let length = (stream.output_end - stream.output_start).min(output.remaining());
                output.put_slice(&stream.output[stream.output_start..stream.output_start + length]);
                stream.output_start += length;
                return Poll::Ready(Ok(()));
            }
            if stream.finished || !ready!(stream.poll_packet(context))? {
                return Poll::Ready(Ok(()));
            }
            stream.process_packet()?;
        }
    }
}

fn payload_start(packet: &[u8; TS_PACKET_BYTES]) -> io::Result<Option<usize>> {
    let control = (packet[3] >> 4) & 0x03;
    match control {
        1 => Ok(Some(4)),
        3 => {
            let start = 5 + usize::from(packet[4]);
            (start <= TS_PACKET_BYTES)
                .then_some(Some(start))
                .ok_or_else(|| io::Error::other("invalid MPEG-TS adaptation field"))
        }
        2 => Ok(None),
        _ => Err(io::Error::other("invalid MPEG-TS adaptation control")),
    }
}

fn section(payload: &[u8], starts_unit: bool) -> Option<&[u8]> {
    if !starts_unit {
        return None;
    }
    let pointer = usize::from(*payload.first()?);
    payload.get(pointer + 1..)
}

fn parse_pat(payload: &[u8], starts_unit: bool) -> Option<u16> {
    let section = section(payload, starts_unit)?;
    if section.first().copied()? != 0 {
        return None;
    }
    let section_length = (usize::from(section.get(1)? & 0x0f) << 8) | usize::from(*section.get(2)?);
    let end = 3usize.checked_add(section_length)?.checked_sub(4)?;
    let mut position = 8;
    while position + 4 <= end && position + 4 <= section.len() {
        let program = u16::from_be_bytes([section[position], section[position + 1]]);
        let pid = (u16::from(section[position + 2] & 0x1f) << 8) | u16::from(section[position + 3]);
        if program != 0 {
            return Some(pid);
        }
        position += 4;
    }
    None
}

fn parse_pmt(payload: &[u8], starts_unit: bool) -> Option<u16> {
    let section = section(payload, starts_unit)?;
    if section.first().copied()? != 2 {
        return None;
    }
    let section_length = (usize::from(section.get(1)? & 0x0f) << 8) | usize::from(*section.get(2)?);
    let end = 3usize.checked_add(section_length)?.checked_sub(4)?;
    let program_info_length =
        (usize::from(section.get(10)? & 0x0f) << 8) | usize::from(*section.get(11)?);
    let mut position = 12usize.checked_add(program_info_length)?;
    while position + 5 <= end && position + 5 <= section.len() {
        let stream_type = section[position];
        let pid = (u16::from(section[position + 1] & 0x1f) << 8) | u16::from(section[position + 2]);
        let info_length =
            (usize::from(section[position + 3] & 0x0f) << 8) | usize::from(section[position + 4]);
        if stream_type == STREAM_TYPE_ADTS {
            return Some(pid);
        }
        position = position.checked_add(5 + info_length)?;
    }
    None
}

fn pes_payload(payload: &[u8]) -> Option<(&[u8], Option<usize>)> {
    if payload.get(..3)? != [0, 0, 1] {
        return None;
    }
    let packet_length = usize::from(u16::from_be_bytes([*payload.get(4)?, *payload.get(5)?]));
    let header_length = usize::from(*payload.get(8)?);
    let data = payload.get(9 + header_length..)?;
    if packet_length == 0 {
        return Some((data, None));
    }
    let total = packet_length.checked_sub(3 + header_length)?;
    let length = total.min(data.len());
    Some((&data[..length], Some(total - length)))
}

fn parse_sdt(payload: &[u8], starts_unit: bool) -> Option<(String, String)> {
    let section = section(payload, starts_unit)?;
    if !matches!(section.first().copied()?, 0x42 | 0x46) {
        return None;
    }
    let section_length = (usize::from(section.get(1)? & 0x0f) << 8) | usize::from(*section.get(2)?);
    let end = 3usize.checked_add(section_length)?.checked_sub(4)?;
    let mut service = 11;
    while service + 5 <= end && service + 5 <= section.len() {
        let descriptors_length =
            (usize::from(section[service + 3] & 0x0f) << 8) | usize::from(section[service + 4]);
        let descriptors_end = (service + 5 + descriptors_length).min(section.len());
        let mut descriptor = service + 5;
        while descriptor + 2 <= descriptors_end {
            let length = usize::from(section[descriptor + 1]);
            let body_start = descriptor + 2;
            let body_end = body_start.checked_add(length)?;
            if body_end > descriptors_end {
                break;
            }
            if section[descriptor] == 0x48 {
                let body = &section[body_start..body_end];
                let provider_length = usize::from(*body.get(1)?);
                let provider_end = 2usize.checked_add(provider_length)?;
                let author = std::str::from_utf8(body.get(2..provider_end)?).ok()?;
                let title_length = usize::from(*body.get(provider_end)?);
                let title_start = provider_end + 1;
                let title_end = title_start.checked_add(title_length)?;
                let title = std::str::from_utf8(body.get(title_start..title_end)?).ok()?;
                return Some((author.to_owned(), title.to_owned()));
            }
            descriptor = body_end;
        }
        service = descriptors_end;
    }
    None
}

pub(crate) fn looks_like_ts(bytes: &[u8]) -> bool {
    bytes.len() >= TS_PACKET_BYTES
        && bytes.first() == Some(&0x47)
        && (bytes.len() < TS_PACKET_BYTES * 2 || bytes.get(TS_PACKET_BYTES) == Some(&0x47))
}

#[cfg(test)]
#[path = "../../../tests/source_mpegts.rs"]
mod tests;
