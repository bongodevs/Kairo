use std::io::{Read, Seek, SeekFrom};
use std::pin::Pin;

use symphonia::core::io::MediaSource;
use tokio::runtime::Handle;
use tokio_util::io::SyncIoBridge;

use kairo_core::{MediaStream, OpenedSource};

pub(super) struct DecoderInput {
    bridge: SyncIoBridge<Pin<Box<dyn MediaStream>>>,
    seekable: bool,
    length: Option<u64>,
}

impl DecoderInput {
    pub(super) fn new(source: OpenedSource, runtime: Handle) -> Self {
        let seekable = source.is_seekable();
        let length = source.content_length();
        let bridge = SyncIoBridge::new_with_handle(source.into_stream(), runtime);
        Self { bridge, seekable, length }
    }
}

impl Read for DecoderInput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.bridge.read(buffer)
    }
}

impl Seek for DecoderInput {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.bridge.seek(position)
    }
}

impl MediaSource for DecoderInput {
    fn is_seekable(&self) -> bool {
        self.seekable
    }

    fn byte_len(&self) -> Option<u64> {
        self.length
    }
}
