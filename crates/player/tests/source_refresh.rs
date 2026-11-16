use std::io::{self, Cursor, SeekFrom};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use kairo_core::track::Track;

use super::*;
use crate::source::OpenFuture;
use crate::source::http::gone_for_test;
use crate::source::{LoadFuture, Source, SourceResult, TrackSource};

/// A seekable stream that yields `budget` bytes, then reports an expired URL.
struct FailingStream {
    data: Cursor<Vec<u8>>,
    budget: usize,
    status: u16,
    failed: bool,
}

impl FailingStream {
    fn new(data: Vec<u8>, budget: usize, status: u16) -> Self {
        Self { data: Cursor::new(data), budget, status, failed: false }
    }
}

impl AsyncRead for FailingStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.failed {
            return Poll::Ready(Err(gone_for_test(self.status)));
        }
        if self.budget == 0 {
            self.failed = true;
            return Poll::Ready(Err(gone_for_test(self.status)));
        }
        let limit = self.budget.min(output.remaining()).min(64);
        let mut scratch = [0; 64];
        let mut buffer = ReadBuf::new(&mut scratch[..limit]);
        ready!(Pin::new(&mut self.data).poll_read(context, &mut buffer))?;
        let read = buffer.filled().len();
        if read == 0 {
            self.failed = true;
            return Poll::Ready(Err(gone_for_test(self.status)));
        }
        self.budget -= read;
        output.put_slice(buffer.filled());
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for FailingStream {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        Pin::new(&mut self.data).start_seek(position)
    }

    fn poll_complete(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Pin::new(&mut self.data).poll_complete(context)
    }
}

/// First open returns a stream that expires; later opens return the whole media.
struct ExpiringSource {
    opens: Arc<AtomicUsize>,
    data: Vec<u8>,
    budget: usize,
    always_expire: bool,
}

impl Source for ExpiringSource {
    fn name(&self) -> &str {
        "expiring"
    }

    fn claims(&self, identifier: &str) -> bool {
        identifier.starts_with("expiring:")
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async move {
            let open = self.opens.fetch_add(1, Ordering::Relaxed) + 1;
            let stream: Pin<Box<dyn MediaStream>> = if open == 1 {
                Box::pin(FailingStream::new(self.data.clone(), self.budget, 410))
            } else if self.always_expire {
                Box::pin(FailingStream::new(self.data.clone(), 0, 410))
            } else {
                Box::pin(Cursor::new(self.data.clone()))
            };
            Ok(SourceResult::Opened(OpenedSource::seekable(
                "expiring",
                reference.clone(),
                SourceHints::default(),
                Some(self.data.len() as u64),
                stream,
            )))
        })
    }
}

impl TrackSource for ExpiringSource {
    fn open<'a>(&'a self, _: &'a Track) -> OpenFuture<'a> {
        Box::pin(async { Ok(None) })
    }
}

fn source(opens: Arc<AtomicUsize>, budget: usize, always_expire: bool) -> Sources {
    let mut sources = Sources::new();
    sources
        .register(ExpiringSource { opens, data: b"0123456789".to_vec(), budget, always_expire })
        .expect("register expiring source");
    sources
}

#[tokio::test]
async fn an_expired_url_is_reopened_once_and_resumes_at_the_current_byte() {
    let opens = Arc::new(AtomicUsize::new(0));
    let sources = source(Arc::clone(&opens), 4, false);
    let mut opened =
        sources.open(SourceReference::new("expiring:track")).await.expect("open").expect("media");

    let mut first = [0; 4];
    opened.read(&mut first).await.expect("first read");
    assert_eq!(&first, b"0123");

    let mut rest = [0; 6];
    opened.read(&mut rest).await.expect("re-resolved read");
    assert_eq!(&rest, b"456789");
    assert_eq!(opens.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn a_seek_moves_the_byte_the_fresh_stream_resumes_from() {
    let opens = Arc::new(AtomicUsize::new(0));
    let sources = source(Arc::clone(&opens), 6, false);
    let mut opened =
        sources.open(SourceReference::new("expiring:track")).await.expect("open").expect("media");

    let mut first = [0; 2];
    opened.read(&mut first).await.expect("first read");
    assert_eq!(&first, b"01");

    opened.seek(SeekFrom::Start(4)).await.expect("seek");
    let mut middle = [0; 4];
    opened.read(&mut middle).await.expect("middle read");
    assert_eq!(&middle, b"4567");

    let mut rest = [0; 2];
    opened.read(&mut rest).await.expect("re-resolved read");
    assert_eq!(&rest, b"89");
    assert_eq!(opens.load(Ordering::Relaxed), 2);
}

#[test]
fn only_refusal_and_removal_mark_the_url_gone() {
    use crate::source::http::{gone_for_test, source_gone};

    assert!(source_gone(&gone_for_test(403)));
    assert!(source_gone(&gone_for_test(410)));
    assert!(!source_gone(&gone_for_test(404)));
    assert!(!source_gone(&io::Error::other("plain transport failure")));
}

#[tokio::test]
async fn a_second_expired_url_is_a_plain_read_error() {
    let opens = Arc::new(AtomicUsize::new(0));
    let sources = source(Arc::clone(&opens), 4, true);
    let mut opened =
        sources.open(SourceReference::new("expiring:track")).await.expect("open").expect("media");

    let mut first = [0; 4];
    opened.read(&mut first).await.expect("first read");

    let mut rest = [0; 6];
    let error = opened.read(&mut rest).await.expect_err("second refusal fails cleanly");
    assert!(crate::source::http::source_gone(&error), "the HTTP status stays visible");
    assert_eq!(opens.load(Ordering::Relaxed), 2);
}
