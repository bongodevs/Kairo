use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io::{self, SeekFrom};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use futures_util::TryStreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::{Client, Response, Url};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, ReadBuf};
use tokio_util::io::StreamReader;

const CONNECT_ATTEMPTS: usize = 2;
/// Consecutive body reconnects allowed before the stream surfaces the failure.
const MAX_RECONNECTS: u8 = 4;
/// Successful bytes that clear the reconnect budget, roughly one skip buffer.
const RECONNECT_RESET_BYTES: u64 = 8 * 1024;
/// Resident bytes the range cache may hold per stream.
pub(super) const RANGE_CACHE_BYTES: usize = 256 * 1024;

/// A cached span of the response body, addressed by its absolute offset.
struct CachedRange {
    start: u64,
    bytes: Vec<u8>,
    /// Bytes at the front of `bytes` that have already been evicted.
    head: usize,
    used: u64,
}

impl CachedRange {
    fn new(start: u64, bytes: &[u8], used: u64) -> Self {
        Self { start, bytes: bytes.to_vec(), head: 0, used }
    }

    fn end(&self) -> u64 {
        self.start.saturating_add(self.len() as u64)
    }

    fn len(&self) -> usize {
        self.bytes.len() - self.head
    }

    fn live(&self) -> &[u8] {
        &self.bytes[self.head..]
    }

    fn drop_front(&mut self, count: usize) {
        self.start = self.start.saturating_add(count as u64);
        self.head += count;
        if self.head == self.bytes.len() {
            self.bytes.clear();
            self.head = 0;
        } else if self.head * 2 >= self.bytes.len() {
            self.bytes.drain(..self.head);
            self.head = 0;
        }
    }
}

/// Bounded least-recently-used cache of body bytes.
///
/// Container probing seeks back over data it has already read (an MP4 `moov` at the end is read
/// twice). Serving those reads from here avoids a new request and leaves the open body untouched.
struct RangeCache {
    ranges: VecDeque<CachedRange>,
    limit: usize,
    bytes: usize,
    clock: u64,
}

impl RangeCache {
    fn new(limit: usize) -> Self {
        Self { ranges: VecDeque::new(), limit, bytes: 0, clock: 0 }
    }

    fn contains(&self, position: u64) -> bool {
        self.ranges.iter().any(|range| range.start <= position && position < range.end())
    }

    /// Copy cached bytes at `position` into `output`, returning how many were available.
    fn read_at(&mut self, position: u64, output: &mut [u8]) -> usize {
        let Some(range) =
            self.ranges.iter_mut().find(|range| range.start <= position && position < range.end())
        else {
            return 0;
        };
        let offset = (position - range.start) as usize;
        let length = (range.len() - offset).min(output.len());
        output[..length].copy_from_slice(&range.live()[offset..offset + length]);
        self.clock += 1;
        range.used = self.clock;
        length
    }

    /// Record bytes read from the body at `start`.
    fn insert(&mut self, start: u64, bytes: &[u8]) {
        if bytes.is_empty() || self.limit == 0 {
            return;
        }
        let end = start.saturating_add(bytes.len() as u64);
        let mut kept = VecDeque::with_capacity(self.ranges.len());
        for range in self.ranges.drain(..) {
            if range.end() <= start || range.start >= end {
                kept.push_back(range);
            }
        }
        self.ranges = kept;
        self.clock += 1;
        self.ranges.push_back(CachedRange::new(start, bytes, self.clock));
        self.merge();
        self.bytes = self.ranges.iter().map(CachedRange::len).sum();
        self.evict();
    }

    /// Join ranges that became adjacent so reads stay contiguous.
    fn merge(&mut self) {
        self.ranges.make_contiguous().sort_unstable_by_key(|range| range.start);
        let mut merged: VecDeque<CachedRange> = VecDeque::with_capacity(self.ranges.len());
        for range in self.ranges.drain(..) {
            if let Some(last) = merged.back_mut()
                && last.end() == range.start
            {
                last.bytes.extend_from_slice(range.live());
                last.used = last.used.max(range.used);
                continue;
            }
            merged.push_back(range);
        }
        self.ranges = merged;
    }

    /// Drop the least recently used bytes until the cache is back inside its limit.
    fn evict(&mut self) {
        while self.bytes > self.limit {
            let Some(index) = self
                .ranges
                .iter()
                .enumerate()
                .min_by_key(|(_, range)| range.used)
                .map(|(index, _)| index)
            else {
                break;
            };
            let excess = self.bytes - self.limit;
            let range = &mut self.ranges[index];
            let drop = excess.min(range.len());
            range.drop_front(drop);
            self.bytes -= drop;
            if range.len() == 0 {
                self.ranges.remove(index);
            }
        }
    }
}

/// An HTTP status that says the URL itself is no longer usable (expired or removed).
#[derive(Debug)]
struct SourceGoneError {
    message: String,
    status: u16,
}

impl fmt::Display for SourceGoneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SourceGoneError {}

fn status_error(message: String, status: u16) -> io::Error {
    io::Error::other(SourceGoneError { message, status })
}

/// Whether this error is the host HTTP stack refusing or forgetting the media URL.
pub(crate) fn source_gone(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<SourceGoneError>())
        .is_some_and(|gone| matches!(gone.status, 403 | 410))
}

#[cfg(test)]
pub(crate) fn gone_for_test(status: u16) -> io::Error {
    status_error(format!("HTTP request returned status {status}"), status)
}

/// Body errors worth a reconnect. The reqwest stream maps hyper failures to `Other`, so that kind
/// is retriable too; HTTP status and range errors are rejected before a body exists.
fn is_retriable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::Other
    )
}

pub(super) type Body = Pin<Box<dyn AsyncRead + Send + Sync>>;

pub(super) fn response_body(response: Response) -> Body {
    let stream = response.bytes_stream().map_err(io::Error::other);
    Box::pin(StreamReader::new(Box::pin(stream)))
}

pub(super) async fn read_bounded(
    mut bytes: Vec<u8>,
    mut body: Body,
    maximum: usize,
) -> io::Result<Vec<u8>> {
    if bytes.len() > maximum {
        return Err(io::Error::other("HTTP response exceeds the byte limit"));
    }
    let mut chunk = [0; 8 * 1024];
    while bytes.len() <= maximum {
        let remaining = maximum + 1 - bytes.len();
        let length = remaining.min(chunk.len());
        let read = body.read(&mut chunk[..length]).await?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Err(io::Error::other("HTTP response exceeds the byte limit"))
}

pub(super) struct OpenedBody {
    pub(super) body: Body,
    pub(super) content_length: Option<u64>,
}

pub(super) async fn open_at(
    client: Client,
    url: Url,
    position: u64,
    known_length: Option<u64>,
) -> io::Result<OpenedBody> {
    for attempt in 0..CONNECT_ATTEMPTS {
        let mut request = client.get(url.clone());
        if position > 0 {
            request = request.header(RANGE, format!("bytes={position}-"));
        }
        let response = request.send().await.map_err(io::Error::other)?;
        let status = response.status().as_u16();
        if status >= 500 && attempt + 1 < CONNECT_ATTEMPTS {
            continue;
        }
        if position > 0 {
            if status != 206 {
                return Err(status_error(
                    format!("HTTP range request returned status {status}"),
                    status,
                ));
            }
            let (start, total) = content_range(&response)?;
            if start != position {
                return Err(io::Error::other("HTTP range started at the wrong position"));
            }
            return Ok(OpenedBody {
                body: response_body(response),
                content_length: total.or(known_length),
            });
        }
        if !matches!(status, 200 | 203 | 206) {
            return Err(status_error(format!("HTTP request returned status {status}"), status));
        }
        let content_length = response.content_length().or(known_length);
        return Ok(OpenedBody { body: response_body(response), content_length });
    }
    Err(io::Error::other("HTTP connection attempts exhausted"))
}

fn content_range(response: &Response) -> io::Result<(u64, Option<u64>)> {
    let value = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| io::Error::other("HTTP range response has no Content-Range"))?;
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| io::Error::other("invalid HTTP Content-Range"))?;
    let (range, total) =
        value.split_once('/').ok_or_else(|| io::Error::other("invalid HTTP Content-Range"))?;
    let (start, _) =
        range.split_once('-').ok_or_else(|| io::Error::other("invalid HTTP Content-Range"))?;
    let start = start.parse().map_err(|_| io::Error::other("invalid HTTP Content-Range start"))?;
    let total = match total {
        "*" => None,
        total => {
            Some(total.parse().map_err(|_| io::Error::other("invalid HTTP Content-Range total"))?)
        }
    };
    Ok((start, total))
}

type OpenFuture = Pin<Box<dyn Future<Output = io::Result<OpenedBody>> + Send + Sync>>;

enum ReadState {
    Reading(Body),
    Opening { position: u64, future: OpenFuture },
}

#[derive(Clone, Copy)]
enum SeekAction {
    Skip(u64),
    Reopen(u64),
}

pub(super) struct HttpStream {
    client: Client,
    url: Url,
    content_length: Option<u64>,
    /// The logical read position the caller sees.
    position: u64,
    /// Absolute offset of the next byte the current body will yield.
    body_position: u64,
    state: ReadState,
    seek: Option<SeekAction>,
    reconnects: u8,
    reconnect_progress: u64,
    /// The primed prefix, kept for container detection.
    replay: Vec<u8>,
    cache: RangeCache,
}

impl HttpStream {
    pub(super) fn new(client: Client, url: Url, body: Body, content_length: Option<u64>) -> Self {
        Self {
            client,
            url,
            content_length,
            position: 0,
            body_position: 0,
            state: ReadState::Reading(body),
            seek: None,
            reconnects: 0,
            reconnect_progress: 0,
            replay: Vec::new(),
            cache: RangeCache::new(RANGE_CACHE_BYTES),
        }
    }

    pub(super) async fn prime(&mut self, maximum: usize) -> io::Result<()> {
        if !self.replay.is_empty() {
            return Ok(());
        }
        let maximum = self
            .content_length
            .and_then(|length| usize::try_from(length).ok())
            .map_or(maximum, |length| length.min(maximum));
        let mut prefix = vec![0; maximum];
        let mut length = 0;
        while length < maximum {
            let mut output = ReadBuf::new(&mut prefix[length..]);
            let result = std::future::poll_fn(|context| self.poll_head(context, &mut output)).await;
            if let Err(error) = result {
                if length == 0 {
                    return Err(error);
                }
                break;
            }
            let read = output.filled().len();
            if read == 0 {
                break;
            }
            length += read;
        }
        prefix.truncate(length);
        self.cache.insert(0, &prefix);
        self.replay = prefix;
        self.position = 0;
        Ok(())
    }

    fn poll_head(
        &mut self,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        ready!(self.poll_open(context))?;
        let before = output.filled().len();
        let result = match &mut self.state {
            ReadState::Reading(body) => body.as_mut().poll_read(context, output),
            ReadState::Opening { .. } => return Poll::Pending,
        };
        if let Poll::Ready(Ok(())) = &result {
            let read = output.filled().len() - before;
            self.position = self.position.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            self.body_position = self.position;
        }
        result
    }

    pub(super) fn prefix(&self) -> &[u8] {
        &self.replay
    }

    fn begin_open(&mut self, position: u64) {
        let client = self.client.clone();
        let url = self.url.clone();
        let content_length = self.content_length;
        self.state = ReadState::Opening {
            position,
            future: Box::pin(open_at(client, url, position, content_length)),
        };
    }

    fn note_progress(&mut self, read: usize) {
        self.reconnect_progress =
            self.reconnect_progress.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if self.reconnect_progress >= RECONNECT_RESET_BYTES {
            self.reconnect_progress = 0;
            self.reconnects = 0;
        }
    }

    /// Start one bounded reconnect. Returns false once the budget is spent.
    fn begin_reconnect(&mut self) -> bool {
        if self.reconnects >= MAX_RECONNECTS {
            return false;
        }
        self.reconnects += 1;
        self.reconnect_progress = 0;
        self.begin_open(self.position);
        true
    }

    fn poll_open(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let ReadState::Opening { position, future } = &mut self.state else {
            return Poll::Ready(Ok(()));
        };
        let target = *position;
        match future.as_mut().poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(opened)) => {
                self.position = target;
                self.body_position = target;
                self.content_length = opened.content_length.or(self.content_length);
                self.state = ReadState::Reading(opened.body);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.body_position = self.position;
                self.state = ReadState::Reading(Box::pin(tokio::io::empty()));
                Poll::Ready(Err(error))
            }
        }
    }

    fn poll_content(
        &mut self,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            ready!(self.poll_open(context))?;
            if self.position != self.body_position {
                let position = self.position;
                let length = self.cache.read_at(position, output.initialize_unfilled());
                if length > 0 {
                    output.advance(length);
                    self.position = self.position.saturating_add(length as u64);
                    return Poll::Ready(Ok(()));
                }
                if self.content_length == Some(position) {
                    return Poll::Ready(Ok(()));
                }
                self.begin_open(position);
                continue;
            }
            let before = output.filled().len();
            let result = match &mut self.state {
                ReadState::Reading(body) => body.as_mut().poll_read(context, output),
                ReadState::Opening { .. } => continue,
            };
            match result {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(())) => {
                    let read = output.filled().len() - before;
                    if read > 0 {
                        let start = self.position;
                        self.cache.insert(start, &output.filled()[before..]);
                        self.position =
                            self.position.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
                        self.body_position = self.position;
                        self.note_progress(read);
                        return Poll::Ready(Ok(()));
                    }
                    if self.content_length.is_some_and(|length| self.position < length) {
                        if self.position > 0 && self.begin_reconnect() {
                            continue;
                        }
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "HTTP body ended before the declared length",
                        )));
                    }
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Err(error)) => {
                    if self.position > 0 && is_retriable(&error) && self.begin_reconnect() {
                        continue;
                    }
                    return Poll::Ready(Err(error));
                }
            }
        }
    }
}

impl AsyncRead for HttpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let stream = self.get_mut();
        if stream.seek.is_some() {
            return Poll::Ready(Err(io::Error::other("HTTP seek is still pending")));
        }
        stream.poll_content(context, output)
    }
}

const MAX_SKIP_DISTANCE: u64 = 512 * 1024;
const SKIP_BUFFER: usize = 8 * 1024;

impl HttpStream {
    fn seek_target(&self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => {
                let length = self.content_length.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "HTTP length is unknown")
                })?;
                i128::from(length) + i128::from(offset)
            }
        };
        let target = u64::try_from(target)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid HTTP seek"))?;
        if self.content_length.is_some_and(|length| target > length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HTTP seek is beyond the response length",
            ));
        }
        Ok(target)
    }
}

impl AsyncSeek for HttpStream {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        let stream = self.get_mut();
        if stream.seek.is_some() {
            return Err(io::Error::other("another HTTP seek is still pending"));
        }
        let target = stream.seek_target(position)?;
        let current = stream.position;
        if target == current {
            return Ok(());
        }
        let pending_open = matches!(stream.state, ReadState::Opening { .. });
        if pending_open {
            stream.body_position = current;
            stream.state = ReadState::Reading(Box::pin(tokio::io::empty()));
        }
        if stream.content_length == Some(target) {
            stream.position = target;
            stream.seek = None;
            return Ok(());
        }
        if stream.cache.contains(target) {
            stream.position = target;
            stream.seek = None;
            return Ok(());
        }
        stream.seek = if !pending_open
            && target > current
            && target - current <= MAX_SKIP_DISTANCE
            && target > stream.body_position
        {
            stream.position = current.max(stream.body_position);
            Some(SeekAction::Skip(target))
        } else if stream.content_length.is_some() {
            Some(SeekAction::Reopen(target))
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "HTTP source does not support this seek",
            ));
        };
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        let stream = self.get_mut();
        loop {
            match stream.seek {
                None => return Poll::Ready(Ok(stream.position)),
                Some(SeekAction::Reopen(target)) => {
                    if !matches!(stream.state, ReadState::Opening { position, .. } if position == target)
                    {
                        stream.reconnects = 0;
                        stream.reconnect_progress = 0;
                        stream.begin_open(target);
                    }
                    match stream.poll_open(context) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(())) => {}
                        Poll::Ready(Err(error)) => {
                            stream.seek = None;
                            return Poll::Ready(Err(error));
                        }
                    }
                    stream.seek = None;
                }
                Some(SeekAction::Skip(target)) => {
                    let mut scratch = [0; SKIP_BUFFER];
                    while stream.position < target {
                        let remaining = usize::try_from(target - stream.position)
                            .unwrap_or(usize::MAX)
                            .min(scratch.len());
                        let mut output = ReadBuf::new(&mut scratch[..remaining]);
                        ready!(stream.poll_content(context, &mut output))?;
                        if output.filled().is_empty() {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "HTTP seek reached end of stream",
                            )));
                        }
                    }
                    stream.seek = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RangeCache;

    fn read(cache: &mut RangeCache, position: u64, length: usize) -> Option<Vec<u8>> {
        let mut output = vec![0; length];
        let read = cache.read_at(position, &mut output);
        (read > 0).then(|| output[..read].to_vec())
    }

    #[test]
    fn cached_bytes_are_served_by_offset() {
        let mut cache = RangeCache::new(64);
        cache.insert(10, b"hello");

        assert_eq!(read(&mut cache, 10, 8), Some(b"hello".to_vec()));
        assert_eq!(read(&mut cache, 12, 8), Some(b"llo".to_vec()));
        assert_eq!(read(&mut cache, 15, 8), None);
        assert_eq!(read(&mut cache, 9, 8), None);
    }

    #[test]
    fn adjacent_reads_merge_into_one_span() {
        let mut cache = RangeCache::new(64);
        cache.insert(0, b"abcd");
        cache.insert(4, b"efgh");

        assert!(cache.contains(0));
        assert!(cache.contains(7));
        assert_eq!(read(&mut cache, 0, 8), Some(b"abcdefgh".to_vec()));
    }

    #[test]
    fn the_cache_stays_inside_its_limit() {
        let mut cache = RangeCache::new(8);
        cache.insert(0, b"abcd");
        cache.insert(4, b"efgh");
        cache.insert(8, b"ij");

        assert!(!cache.contains(0));
        assert_eq!(read(&mut cache, 2, 8), Some(b"cdefghij".to_vec()));
        assert_eq!(read(&mut cache, 10, 8), None);
        assert!(cache.bytes <= cache.limit);
    }

    #[test]
    fn the_least_recently_used_span_is_evicted_first() {
        let mut cache = RangeCache::new(8);
        cache.insert(10, b"abcd");
        cache.insert(20, b"efgh");
        assert_eq!(read(&mut cache, 10, 4), Some(b"abcd".to_vec()));
        cache.insert(30, b"ij");

        assert_eq!(read(&mut cache, 10, 4), Some(b"abcd".to_vec()));
        assert_eq!(read(&mut cache, 20, 4), None);
        assert_eq!(read(&mut cache, 22, 4), Some(b"gh".to_vec()));
        assert_eq!(read(&mut cache, 30, 4), Some(b"ij".to_vec()));
    }
}
