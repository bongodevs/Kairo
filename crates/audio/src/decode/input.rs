use std::io::{Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use symphonia::core::io::MediaSource;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
use tokio::runtime::Handle;
use tokio_util::io::SyncIoBridge;

use kairo_core::{MediaStream, OpenedSource};

/// Shared handle and waker allowing a worker to cancel blocking read or seek operations immediately.
#[derive(Debug, Default)]
pub struct InterruptHandle {
    interrupted: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl InterruptHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        if let Ok(mut lock) = self.waker.lock()
            && let Some(waker) = lock.take()
        {
            waker.wake();
        }
    }

    pub fn reset(&self) {
        self.interrupted.store(false, Ordering::Release);
    }

    #[must_use]
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }
}

struct InterruptibleStream {
    stream: Pin<Box<dyn MediaStream>>,
    interrupt: Arc<InterruptHandle>,
}

impl AsyncRead for InterruptibleStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.interrupt.is_interrupted() {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "read interrupted by worker signal",
            )));
        }
        if let Ok(mut lock) = self.interrupt.waker.lock() {
            *lock = Some(cx.waker().clone());
        }
        match Pin::new(&mut self.stream).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if let Ok(mut lock) = self.interrupt.waker.lock() {
                    *lock = None;
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                if self.interrupt.is_interrupted() {
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "read interrupted by worker signal",
                    )))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

impl AsyncSeek for InterruptibleStream {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        Pin::new(&mut self.stream).start_seek(position)
    }

    fn poll_complete(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Pin::new(&mut self.stream).poll_complete(cx)
    }
}

pub(super) struct DecoderInput {
    bridge: SyncIoBridge<InterruptibleStream>,
    seekable: bool,
    length: Option<u64>,
}

impl DecoderInput {
    pub(super) fn new(source: OpenedSource, runtime: Handle) -> (Self, Arc<InterruptHandle>) {
        let seekable = source.is_seekable();
        let length = source.content_length();
        let interrupt = Arc::new(InterruptHandle::new());
        let stream =
            InterruptibleStream { stream: source.into_stream(), interrupt: Arc::clone(&interrupt) };
        let bridge = SyncIoBridge::new_with_handle(stream, runtime);
        (Self { bridge, seekable, length }, interrupt)
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
