use std::fmt;
use std::io::{self, SeekFrom};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, ReadBuf};

use crate::track::TrackInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReference {
    identifier: String,
    title: Option<String>,
}

impl SourceReference {
    #[must_use]
    pub fn new(identifier: impl Into<String>) -> Self {
        Self { identifier: identifier.into(), title: None }
    }

    #[must_use]
    pub fn with_title(identifier: impl Into<String>, title: impl Into<String>) -> Self {
        Self { identifier: identifier.into(), title: Some(title.into()) }
    }

    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn replacing_identifier(&self, identifier: impl Into<String>) -> Self {
        Self { identifier: identifier.into(), title: self.title.clone() }
    }
}

/// Hints passed to container detection without asserting that either is correct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceHints {
    pub mime_type: Option<String>,
    pub file_extension: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
}

impl From<&TrackInfo> for SourceMetadata {
    /// The metadata a track advertises to a stream it opens.
    fn from(info: &TrackInfo) -> Self {
        Self {
            title: Some(info.title.clone()),
            author: Some(info.author.clone()),
            artwork_url: info.artwork_url.clone(),
            isrc: info.isrc.clone(),
        }
    }
}

pub type SourceMetadataUpdates = Arc<Mutex<SourceMetadata>>;

/// A single-owner media byte stream moved onto one decode worker.
pub trait MediaStream: AsyncRead + AsyncSeek + Send + Sync {}

impl<T> MediaStream for T where T: AsyncRead + AsyncSeek + Send + Sync {}

/// A source selected by the registry, ready for the demuxer to consume.
pub struct OpenedSource {
    source_name: Box<str>,
    reference: SourceReference,
    hints: SourceHints,
    metadata: SourceMetadata,
    metadata_updates: Option<SourceMetadataUpdates>,
    content_length: Option<u64>,
    seekable: bool,
    stream: Pin<Box<dyn MediaStream>>,
}

impl OpenedSource {
    /// Build an opened source whose reader supports seeking.
    pub fn seekable<R>(
        source_name: impl Into<Box<str>>,
        reference: SourceReference,
        hints: SourceHints,
        content_length: Option<u64>,
        reader: R,
    ) -> Self
    where
        R: AsyncRead + AsyncSeek + Send + Sync + 'static,
    {
        let metadata =
            SourceMetadata { title: reference.title.clone(), ..SourceMetadata::default() };
        Self {
            source_name: source_name.into(),
            reference,
            hints,
            metadata,
            metadata_updates: None,
            content_length,
            seekable: true,
            stream: Box::pin(reader),
        }
    }

    /// Build an opened source whose reader is forward-only.
    pub fn read_only<R>(
        source_name: impl Into<Box<str>>,
        reference: SourceReference,
        hints: SourceHints,
        content_length: Option<u64>,
        reader: R,
    ) -> Self
    where
        R: AsyncRead + Send + Sync + Unpin + 'static,
    {
        let metadata =
            SourceMetadata { title: reference.title.clone(), ..SourceMetadata::default() };
        Self {
            source_name: source_name.into(),
            reference,
            hints,
            metadata,
            metadata_updates: None,
            content_length,
            seekable: false,
            stream: Box::pin(ReadOnly::new(reader)),
        }
    }

    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    #[must_use]
    pub fn reference(&self) -> &SourceReference {
        &self.reference
    }

    #[must_use]
    pub fn hints(&self) -> &SourceHints {
        &self.hints
    }

    #[must_use]
    pub fn metadata(&self) -> &SourceMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn with_metadata(mut self, metadata: SourceMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    #[must_use]
    pub fn with_source_name(mut self, source_name: impl Into<Box<str>>) -> Self {
        self.source_name = source_name.into();
        self
    }

    #[must_use]
    pub fn with_reference(mut self, reference: SourceReference) -> Self {
        self.reference = reference;
        self
    }

    /// Replace the transport hints after a source has opened its bytes.
    #[must_use]
    pub fn with_stream_info(
        mut self,
        hints: SourceHints,
        content_length: Option<u64>,
        seekable: bool,
    ) -> Self {
        self.hints = hints;
        self.content_length = content_length;
        self.seekable = seekable;
        self
    }

    pub fn with_metadata_updates(mut self, updates: SourceMetadataUpdates) -> Self {
        self.metadata_updates = Some(updates);
        self
    }

    pub fn metadata_updates(&self) -> Option<SourceMetadataUpdates> {
        self.metadata_updates.clone()
    }

    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    #[must_use]
    pub fn is_seekable(&self) -> bool {
        self.seekable
    }

    pub async fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.stream.as_mut().read(output).await
    }

    pub async fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.stream.as_mut().seek(position).await
    }

    #[must_use]
    pub fn into_stream(self) -> Pin<Box<dyn MediaStream>> {
        self.stream
    }
}

impl fmt::Debug for OpenedSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedSource")
            .field("source_name", &self.source_name)
            .field("reference", &self.reference)
            .field("hints", &self.hints)
            .field("metadata", &self.metadata)
            .field("content_length", &self.content_length)
            .field("seekable", &self.seekable)
            .finish_non_exhaustive()
    }
}

struct ReadOnly<R> {
    inner: R,
}

impl<R> ReadOnly<R> {
    fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R> AsyncRead for ReadOnly<R>
where
    R: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl<R> AsyncSeek for ReadOnly<R>
where
    R: Unpin,
{
    fn start_seek(self: Pin<&mut Self>, _position: SeekFrom) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "source is not seekable"))
    }

    fn poll_complete(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Err(io::Error::new(io::ErrorKind::Unsupported, "source is not seekable")))
    }
}
