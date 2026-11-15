//! The heartbeat clock: when the next beat is due, what it carries, and what the ack said back.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::time::Instant;

use super::client::GatewayError;
use super::socket::Socket;
use super::wire::Frame;
const OP_HEARTBEAT: u8 = 3;

#[derive(Debug)]
pub struct Heartbeat {
    pub(crate) interval: Duration,
    pub(crate) seq_ack: Option<u64>,
    pub(crate) sent_at_ms: Option<u64>,
    pub(crate) ping: Duration,
    shared_ping: Arc<AtomicU64>,
    next: Instant,
}

impl Heartbeat {
    pub(crate) fn new(interval: Duration, seq_ack: Option<u64>) -> Self {
        Self {
            interval,
            seq_ack,
            sent_at_ms: None,
            ping: Duration::ZERO,
            shared_ping: Arc::new(AtomicU64::new(0)),
            next: Instant::now() + interval,
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.next
    }

    pub(crate) fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
        self.next = Instant::now() + interval;
    }

    pub(crate) fn observe(&mut self, seq: u64) {
        self.seq_ack = Some(seq);
    }

    pub(crate) fn acknowledged(&mut self) {
        if let Some(sent_at) = self.sent_at_ms {
            self.ping = Duration::from_millis(epoch_ms().saturating_sub(sent_at));
            self.shared_ping
                .store(u64::try_from(self.ping.as_millis()).unwrap_or(u64::MAX), Ordering::Release);
        }
    }

    pub(crate) fn inherit_ping(&mut self, previous: &Self) {
        self.ping = previous.ping;
        self.shared_ping = Arc::clone(&previous.shared_ping);
    }

    pub(crate) fn ping_handle(&self) -> Ping {
        Ping { value: Arc::clone(&self.shared_ping) }
    }

    pub(crate) async fn send<S: Socket>(&mut self, socket: &mut S) -> Result<(), GatewayError> {
        let now_ms = epoch_ms();
        self.sent_at_ms = Some(now_ms);
        socket.send_text(self.frame_text(now_ms)?).await?;
        let scheduled = self.next + self.interval;
        let now = Instant::now();
        self.next = if scheduled <= now { now + self.interval } else { scheduled };
        Ok(())
    }

    pub(crate) fn frame_text(&self, now_ms: u64) -> Result<String, GatewayError> {
        serde_json::to_string(&Frame {
            op: OP_HEARTBEAT,
            d: HeartbeatMsg { t: now_ms, seq_ack: acked(self.seq_ack) },
        })
        .map_err(GatewayError::from)
    }
}

#[derive(Clone)]
pub struct Ping {
    value: Arc<AtomicU64>,
}

impl Ping {
    #[must_use]
    pub fn millis(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }
}

pub(crate) fn acked(seq_ack: Option<u64>) -> i64 {
    seq_ack.map(|seq| seq as i64).unwrap_or(-1)
}

#[derive(Serialize)]
struct HeartbeatMsg {
    t: u64,
    seq_ack: i64,
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
