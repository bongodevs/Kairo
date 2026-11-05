//! The socket owner: drains the outbound channel, answers pings, and notices a close.

use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use axum::extract::ws::Message as WsMessage;
use futures_util::{Sink, Stream};
use tokio::sync::mpsc;
use tokio::time::{sleep, Sleep};

use crate::protocol::message::Message;

use super::Ended;

// A write that makes no progress for this long closes the socket instead of buffering forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

// One task owns the whole socket. Every loop polls the inbound half first, so a stalled write
// cannot stop a ping from being seen, and a pong is queued ahead of the remaining backlog.
pub(super) async fn pump<S, E>(
    mut socket: S,
    mut receiver: mpsc::Receiver<Message>,
    backlog: Vec<Message>,
    session_id: String,
) -> Ended
where
    S: Stream<Item = Result<WsMessage, E>> + Sink<WsMessage, Error = E> + Unpin,
{
    let mut queued: VecDeque<WsMessage> = backlog.iter().filter_map(encode).collect();
    let mut pending: Option<WsMessage> = None;
    let mut write_deadline: Option<Pin<Box<Sleep>>> = None;
    let mut flush = false;
    let mut live = true;

    poll_fn(|cx| loop {
        let mut progressed = false;

        // Read first: a write waiting on the socket must never hide a ping or a close.
        loop {
            match Pin::new(&mut socket).poll_next(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    progressed = true;
                    match frame {
                        WsMessage::Close(_) => return Poll::Ready(Ended::Read),
                        WsMessage::Ping(_) => {
                            // Tungstenite queues the pong; the read path flushes it ahead of
                            // the application backlog, so only a flush is requested here.
                            flush = true;
                        }
                        WsMessage::Text(_) => tracing::warn!(
                            session = %session_id,
                            "Kairo does not support websocket messages. Please use the REST api."
                        ),
                        WsMessage::Pong(_) | WsMessage::Binary(_) => {}
                    }
                }
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                    return Poll::Ready(Ended::Read)
                }
                Poll::Pending => break,
            }
        }

        if pending.is_none() {
            pending = queued.pop_front();
        }
        if pending.is_none() && live {
            match receiver.poll_recv(cx) {
                Poll::Ready(Some(message)) => {
                    if let Some(frame) = encode(&message) {
                        queued.push_back(frame);
                    }
                    progressed = true;
                }
                Poll::Ready(None) => live = false,
                Poll::Pending => {}
            }
            pending = queued.pop_front();
        }

        if let Some(frame) = pending.take() {
            match Pin::new(&mut socket).poll_ready(cx) {
                Poll::Ready(Ok(())) => {
                    if Pin::new(&mut socket).start_send(frame).is_err() {
                        tracing::warn!(session = %session_id, "websocket write failed, closing the socket");
                        return Poll::Ready(Ended::Write);
                    }
                    flush = true;
                    progressed = true;
                }
                Poll::Ready(Err(_)) => {
                    tracing::warn!(session = %session_id, "websocket write failed, closing the socket");
                    return Poll::Ready(Ended::Write);
                }
                Poll::Pending => pending = Some(frame),
            }
        }

        if flush {
            match Pin::new(&mut socket).poll_flush(cx) {
                Poll::Ready(Ok(())) => {
                    flush = false;
                    // Bytes reached the socket, so this is progress even if more are queued.
                    write_deadline = None;
                    progressed = true;
                }
                Poll::Ready(Err(_)) => {
                    tracing::warn!(session = %session_id, "websocket write failed, closing the socket");
                    return Poll::Ready(Ended::Write);
                }
                Poll::Pending => {}
            }
        }

        if pending.is_some() || flush {
            let deadline =
                write_deadline.get_or_insert_with(|| Box::pin(sleep(WRITE_TIMEOUT)));
            if deadline.as_mut().poll(cx).is_ready() {
                tracing::warn!(
                    session = %session_id,
                    "websocket write timed out, closing the socket"
                );
                return Poll::Ready(Ended::Write);
            }
        } else {
            write_deadline = None;
        }

        if !live && pending.is_none() && queued.is_empty() && !flush {
            return Poll::Ready(Ended::Write);
        }
        if !progressed {
            return Poll::Pending;
        }
    })
    .await
}

fn encode(message: &Message) -> Option<WsMessage> {
    serde_json::to_string(message)
        .ok()
        .map(|text| WsMessage::Text(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Waker};
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct FakeSocket(Arc<FakeInner>);

    #[derive(Default)]
    struct FakeInner {
        inbound: Mutex<VecDeque<Result<WsMessage, Infallible>>>,
        sent: Mutex<Vec<WsMessage>>,
        read_polls: AtomicUsize,
        flushes: AtomicUsize,
        blocked: AtomicBool,
        read_waker: Mutex<Option<Waker>>,
        write_waker: Mutex<Option<Waker>>,
    }

    impl FakeSocket {
        fn block(&self) {
            self.0.blocked.store(true, Ordering::SeqCst);
        }

        fn unblock(&self) {
            self.0.blocked.store(false, Ordering::SeqCst);
            if let Some(waker) = self.0.write_waker.lock().unwrap().take() {
                waker.wake();
            }
        }

        fn push(&self, frame: WsMessage) {
            self.0.inbound.lock().unwrap().push_back(Ok(frame));
            if let Some(waker) = self.0.read_waker.lock().unwrap().take() {
                waker.wake();
            }
        }

        fn sent(&self) -> Vec<WsMessage> {
            self.0.sent.lock().unwrap().clone()
        }

        fn read_polls(&self) -> usize {
            self.0.read_polls.load(Ordering::SeqCst)
        }

        fn flushes(&self) -> usize {
            self.0.flushes.load(Ordering::SeqCst)
        }
    }

    impl Stream for FakeSocket {
        type Item = Result<WsMessage, Infallible>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let inner = &self.0;
            inner.read_polls.fetch_add(1, Ordering::SeqCst);
            if let Some(frame) = inner.inbound.lock().unwrap().pop_front() {
                return Poll::Ready(Some(frame));
            }
            *inner.read_waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    impl Sink<WsMessage> for FakeSocket {
        type Error = Infallible;

        fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            if self.0.blocked.load(Ordering::SeqCst) {
                *self.0.write_waker.lock().unwrap() = Some(cx.waker().clone());
                return Poll::Pending;
            }
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: WsMessage) -> Result<(), Self::Error> {
            self.0.sent.lock().unwrap().push(item);
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.0.flushes.fetch_add(1, Ordering::SeqCst);
            if self.0.blocked.load(Ordering::SeqCst) {
                *self.0.write_waker.lock().unwrap() = Some(cx.waker().clone());
                return Poll::Pending;
            }
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn message(session_id: &str) -> Message {
        Message::Ready {
            resumed: false,
            session_id: session_id.to_string(),
        }
    }

    fn session_of(frame: &WsMessage) -> Option<String> {
        let WsMessage::Text(text) = frame else {
            return None;
        };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        Some(value["sessionId"].as_str().unwrap().to_string())
    }

    async fn wait_for(mut check: impl FnMut() -> bool) {
        for _ in 0..500 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("condition was not reached");
    }

    #[tokio::test]
    async fn backlog_flushes_before_live_messages() {
        let (tx, rx) = mpsc::channel(8);
        let socket = FakeSocket::default();
        let probe = socket.clone();

        tx.send(message("live")).await.unwrap();
        drop(tx);

        let ended = pump(
            socket,
            rx,
            vec![message("one"), message("two")],
            "s".to_string(),
        )
        .await;

        assert!(matches!(ended, Ended::Write));
        let ids: Vec<_> = probe.sent().iter().filter_map(session_of).collect();
        assert_eq!(ids, ["one", "two", "live"]);
    }

    // A write waiting on the socket must not stop the read half from being polled.
    #[tokio::test]
    async fn a_ping_is_read_while_a_write_is_pending() {
        // Kept alive so the channel stays open while the write is blocked.
        let (_tx, rx) = mpsc::channel(8);
        let socket = FakeSocket::default();
        let probe = socket.clone();
        socket.block();

        let task = tokio::spawn(pump(
            socket,
            rx,
            vec![message("one"), message("two")],
            "s".to_string(),
        ));

        // Wait until the first backlog message is waiting on the stalled sink.
        wait_for(|| probe.read_polls() > 0).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(probe.sent().is_empty(), "writes are stalled");

        let polls_before = probe.read_polls();
        let flushes_before = probe.flushes();
        probe.push(WsMessage::Ping(vec![7].into()));
        wait_for(|| probe.read_polls() > polls_before).await;
        wait_for(|| probe.flushes() > flushes_before).await;
        assert!(
            !task.is_finished(),
            "the blocked write must not kill the socket"
        );

        probe.unblock();
        wait_for(|| probe.sent().len() >= 2).await;
        task.abort();

        let ids: Vec<_> = probe.sent().iter().filter_map(session_of).collect();
        assert_eq!(ids, ["one", "two"]);
    }

    #[tokio::test]
    async fn a_closed_stream_ends_the_pump_even_with_a_pending_write() {
        let (_tx, rx) = mpsc::channel(8);
        let socket = FakeSocket::default();
        socket.block();
        socket.push(WsMessage::Close(None));

        let ended = tokio::time::timeout(
            Duration::from_secs(1),
            pump(socket, rx, vec![message("one")], "s".to_string()),
        )
        .await
        .expect("a close must not wait for a blocked write");
        assert!(matches!(ended, Ended::Read));
    }

    #[tokio::test]
    async fn a_closed_channel_ends_the_pump() {
        let (tx, rx) = mpsc::channel(8);
        drop(tx);
        let socket = FakeSocket::default();

        let ended = tokio::time::timeout(
            Duration::from_secs(1),
            pump(socket, rx, Vec::new(), "s".to_string()),
        )
        .await
        .expect("a closed channel ends the socket");
        assert!(matches!(ended, Ended::Write));
    }

    // A write that never completes must close the socket instead of buffering forever.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_write_times_out() {
        let (_tx, rx) = mpsc::channel(8);
        let socket = FakeSocket::default();
        socket.block();

        let task = tokio::spawn(pump(socket, rx, vec![message("one")], "s".to_string()));
        tokio::task::yield_now().await;
        tokio::time::advance(WRITE_TIMEOUT + Duration::from_millis(1)).await;

        for _ in 0..100 {
            if task.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(task.is_finished(), "a stalled write must hit the deadline");
        let ended = task.await.unwrap();
        assert!(matches!(ended, Ended::Write));
    }
}
