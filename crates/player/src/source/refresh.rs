//! One re-resolve for a media URL that the host HTTP stack refuses mid-track.
//!
//! The wrapper sits outside every seekable opened stream, so it covers plugin and built-in sources
//! alike. When the transport reports an expired or removed URL it asks the source engine to open
//! the reference again, seeks the fresh stream to the byte the listener reached and carries on.
//! The second refusal is returned unchanged: the track then fails normally.

use std::future::Future;
use std::io::{self, SeekFrom};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use kairo_core::{MediaStream, OpenedSource, SourceReference};

use super::Sources;
use super::fallback::without_fallback;
use super::http::source_gone;

tokio::task_local! {
    static REFRESHING: bool;
}

/// Whether a fresh stream may still receive its own refresh wrapper.
fn active() -> bool {
    REFRESHING.try_with(|refreshing| !refreshing).unwrap_or(true)
}

async fn without_refresh<F: Future>(future: F) -> F::Output {
    REFRESHING.scope(true, future).await
}

type Refreshed = Pin<Box<dyn MediaStream>>;
type RefreshFuture = Pin<Box<dyn Future<Output = io::Result<Refreshed>> + Send>>;

/// Wrap a seekable opened stream so an expired URL is resolved once more.
pub(super) fn wrap(sources: &Sources, opened: OpenedSource) -> OpenedSource {
    if !opened.is_seekable() || !active() {
        return opened;
    }
    let source_name = opened.source_name().to_owned();
    let reference = opened.reference().clone();
    let hints = opened.hints().clone();
    let content_length = opened.content_length();
    let metadata = opened.metadata().clone();
    let updates = opened.metadata_updates();
    let stream = RefreshStream {
        inner: opened.into_stream(),
        sources: sources.clone(),
        reference: reference.clone(),
        position: 0,
        attempted: false,
        refreshing: false,
        refresh: Mutex::new(None),
    };
    let mut rebuilt = OpenedSource::seekable(source_name, reference, hints, content_length, stream)
        .with_metadata(metadata);
    if let Some(updates) = updates {
        rebuilt = rebuilt.with_metadata_updates(updates);
    }
    rebuilt
}

struct RefreshStream {
    inner: Refreshed,
    sources: Sources,
    reference: SourceReference,
    /// Bytes the caller has consumed, which is where the fresh stream must start.
    position: u64,
    /// One re-resolve per stream.
    attempted: bool,
    /// The refresh future is in the slot and must be polled before the inner stream.
    refreshing: bool,
    refresh: Mutex<Option<RefreshFuture>>,
}

impl RefreshStream {
    fn begin_refresh(&mut self) {
        let sources = self.sources.clone();
        let reference = self.reference.clone();
        let position = self.position;
        self.attempted = true;
        self.refreshing = true;
        *locked_refresh(&self.refresh) = Some(Box::pin(async move {
            let opened = without_refresh(without_fallback(sources.open(reference))).await;
            let mut opened = opened
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("re-resolved source produced no media"))?;
            opened.seek(SeekFrom::Start(position)).await?;
            Ok(opened.into_stream())
        }));
    }
}

fn locked_refresh(
    refresh: &Mutex<Option<RefreshFuture>>,
) -> std::sync::MutexGuard<'_, Option<RefreshFuture>> {
    refresh.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl AsyncRead for RefreshStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.as_mut().get_mut();
        loop {
            if this.refreshing {
                let mut slot = locked_refresh(&this.refresh);
                let Some(future) = slot.as_mut() else {
                    this.refreshing = false;
                    continue;
                };
                match future.as_mut().poll(context) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(stream)) => {
                        this.inner = stream;
                        this.refreshing = false;
                        *slot = None;
                    }
                    Poll::Ready(Err(error)) => {
                        this.refreshing = false;
                        *slot = None;
                        return Poll::Ready(Err(error));
                    }
                }
            }
            let before = output.filled().len();
            match this.inner.as_mut().poll_read(context, output) {
                Poll::Ready(Ok(())) => {
                    let read = output.filled().len() - before;
                    this.position =
                        this.position.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Err(error)) if !this.attempted && source_gone(&error) => {
                    this.begin_refresh();
                }
                other => return other,
            }
        }
    }
}

impl AsyncSeek for RefreshStream {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        let this = self.get_mut();
        this.inner.as_mut().start_seek(position)
    }

    fn poll_complete(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        let this = self.get_mut();
        let result = this.inner.as_mut().poll_complete(context);
        if let Poll::Ready(Ok(position)) = result {
            this.position = position;
        }
        result
    }
}
