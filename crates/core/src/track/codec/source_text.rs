#[cfg(test)]
pub(crate) use self::{
    decode as decode_base64_for_test, encode as encode_base64_for_test,
    text::write as write_text_for_test,
};

use super::TrackCodecError;

#[derive(Debug, Default)]
pub struct SourcePayloadWriter {
    output: Vec<u8>,
}

impl SourcePayloadWriter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn optional_text(&mut self, value: Option<&str>) -> Result<(), TrackCodecError> {
        match value {
            Some(value) => {
                self.output.push(1);
                text::write(&mut self.output, value)
            }
            None => {
                self.output.push(0);
                Ok(())
            }
        }
    }

    pub fn boolean(&mut self, value: bool) {
        self.output.push(u8::from(value));
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.output
    }
}

#[derive(Debug)]
pub struct SourcePayloadReader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> SourcePayloadReader<'a> {
    #[must_use]
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    pub fn optional_text(&mut self) -> Result<Option<String>, TrackCodecError> {
        if self.byte()? == 0 {
            return Ok(None);
        }
        let (value, length) = text::read(self.remaining())?;
        self.position += length;
        Ok(Some(value))
    }

    pub fn boolean(&mut self) -> Result<bool, TrackCodecError> {
        Ok(self.byte()? != 0)
    }

    #[must_use]
    pub const fn consumed(&self) -> usize {
        self.position
    }

    fn byte(&mut self) -> Result<u8, TrackCodecError> {
        let value = *self.input.get(self.position).ok_or(TrackCodecError::Truncated)?;
        self.position += 1;
        Ok(value)
    }

    fn remaining(&self) -> &'a [u8] {
        &self.input[self.position..]
    }
}

pub(super) struct Reader<'a> {
    data: &'a [u8],
    position: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0, end: data.len() }
    }

    pub(super) fn limit_message(&mut self, length: usize) -> Result<(), TrackCodecError> {
        self.position = 4;
        self.end = 4usize.checked_add(length).ok_or(TrackCodecError::Truncated)?;
        if self.end > self.data.len() {
            return Err(TrackCodecError::Truncated);
        }
        Ok(())
    }

    pub(super) fn remaining(&self) -> &'a [u8] {
        &self.data[self.position..self.end]
    }

    pub(super) fn skip(&mut self, length: usize) -> Result<(), TrackCodecError> {
        let _ = self.take(length)?;
        Ok(())
    }

    pub(super) fn byte(&mut self) -> Result<u8, TrackCodecError> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn int(&mut self) -> Result<i32, TrackCodecError> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub(super) fn long(&mut self) -> Result<i64, TrackCodecError> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    pub(super) fn text(&mut self) -> Result<String, TrackCodecError> {
        let (value, length) = text::read(self.remaining())?;
        self.skip(length)?;
        Ok(value)
    }

    pub(super) fn optional_text(&mut self) -> Result<Option<String>, TrackCodecError> {
        match self.byte()? {
            0 => Ok(None),
            _ => self.text().map(Some),
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], TrackCodecError> {
        let end = self.position.checked_add(length).ok_or(TrackCodecError::Truncated)?;
        if end > self.end {
            return Err(TrackCodecError::Truncated);
        }
        let value = &self.data[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], TrackCodecError> {
        self.take(N)?.try_into().map_err(|_| TrackCodecError::Truncated)
    }
}

pub(super) mod text {
    use super::super::TrackCodecError;

    pub(crate) fn write(output: &mut Vec<u8>, text: &str) -> Result<(), TrackCodecError> {
        let start = output.len();
        output.extend_from_slice(&[0, 0]);
        let mut encoded = 0usize;
        for character in text.chars() {
            let codepoint = character as u32;
            if codepoint == 0 {
                output.extend_from_slice(&[0xc0, 0x80]);
                encoded += 2;
            } else if codepoint < 0x80 {
                output.push(codepoint as u8);
                encoded += 1;
            } else if codepoint < 0x800 {
                output.extend_from_slice(&[
                    0xc0 | (codepoint >> 6) as u8,
                    0x80 | (codepoint & 0x3f) as u8,
                ]);
                encoded += 2;
            } else if codepoint < 0x1_0000 {
                encoded += write_three(output, codepoint);
            } else {
                let surrogate = codepoint - 0x1_0000;
                encoded += write_three(output, 0xd800 + (surrogate >> 10));
                encoded += write_three(output, 0xdc00 + (surrogate & 0x3ff));
            }
        }
        if encoded > u16::MAX as usize {
            output.truncate(start);
            return Err(TrackCodecError::FieldTooLong);
        }
        output[start..start + 2].copy_from_slice(&(encoded as u16).to_be_bytes());
        Ok(())
    }

    pub(crate) fn read(input: &[u8]) -> Result<(String, usize), TrackCodecError> {
        let length = u16::from_be_bytes(
            input
                .get(..2)
                .ok_or(TrackCodecError::Truncated)?
                .try_into()
                .map_err(|_| TrackCodecError::Truncated)?,
        ) as usize;
        let bytes = input.get(2..2 + length).ok_or(TrackCodecError::Truncated)?;
        Ok((decode(bytes)?, length + 2))
    }

    fn write_three(output: &mut Vec<u8>, codepoint: u32) -> usize {
        output.extend_from_slice(&[
            0xe0 | (codepoint >> 12) as u8,
            0x80 | ((codepoint >> 6) & 0x3f) as u8,
            0x80 | (codepoint & 0x3f) as u8,
        ]);
        3
    }

    fn decode(bytes: &[u8]) -> Result<String, TrackCodecError> {
        let mut units = Vec::with_capacity(bytes.len());
        let mut offset = 0;
        while offset < bytes.len() {
            let first = bytes[offset];
            if first < 0x80 {
                units.push(u32::from(first));
                offset += 1;
            } else if first & 0xe0 == 0xc0 {
                let second = *bytes.get(offset + 1).ok_or(TrackCodecError::Truncated)?;
                units.push((u32::from(first & 0x1f) << 6) | u32::from(second & 0x3f));
                offset += 2;
            } else if first & 0xf0 == 0xe0 {
                let second = *bytes.get(offset + 1).ok_or(TrackCodecError::Truncated)?;
                let third = *bytes.get(offset + 2).ok_or(TrackCodecError::Truncated)?;
                units.push(
                    (u32::from(first & 0x0f) << 12)
                        | (u32::from(second & 0x3f) << 6)
                        | u32::from(third & 0x3f),
                );
                offset += 3;
            } else {
                return Err(TrackCodecError::InvalidText);
            }
        }

        let mut output = String::new();
        let mut units = units.into_iter().peekable();
        while let Some(codepoint) = units.next() {
            if (0xd800..=0xdbff).contains(&codepoint)
                && let Some(low) = units.peek().copied()
                && (0xdc00..=0xdfff).contains(&low)
            {
                units.next();
                let combined = 0x1_0000 + ((codepoint - 0xd800) << 10) + (low - 0xdc00);
                output.push(char::from_u32(combined).unwrap_or(char::REPLACEMENT_CHARACTER));
            } else {
                output.push(char::from_u32(codepoint).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
        }
        Ok(output)
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn encode(data: &[u8]) -> String {
    let mut output = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let value = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        output.push(ALPHABET[(value >> 18) as usize & 0x3f] as char);
        output.push(ALPHABET[(value >> 12) as usize & 0x3f] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[(value >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 { ALPHABET[value as usize & 0x3f] as char } else { '=' });
    }
    output
}

pub(crate) fn decode(input: &str) -> Result<Vec<u8>, TrackCodecError> {
    if !input.len().is_multiple_of(4) {
        return Err(TrackCodecError::InvalidBase64);
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    for chunk in input.as_bytes().as_chunks::<4>().0 {
        let mut values = [0u32; 4];
        let mut data_len = 4;
        for (index, byte) in chunk.iter().copied().enumerate() {
            if byte == b'=' {
                data_len = data_len.min(index);
                continue;
            }
            if data_len != 4 {
                return Err(TrackCodecError::InvalidBase64);
            }
            values[index] = ALPHABET
                .iter()
                .position(|symbol| *symbol == byte)
                .map(|value| value as u32)
                .ok_or(TrackCodecError::InvalidBase64)?;
        }
        if data_len < 2 {
            return Err(TrackCodecError::InvalidBase64);
        }
        let value = (values[0] << 18) | (values[1] << 12) | (values[2] << 6) | values[3];
        output.push((value >> 16) as u8);
        if data_len > 2 {
            output.push((value >> 8) as u8);
        }
        if data_len > 3 {
            output.push(value as u8);
        }
    }
    Ok(output)
}
