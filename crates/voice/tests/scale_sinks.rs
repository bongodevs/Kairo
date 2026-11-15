use std::net::SocketAddr;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::gaps::{Cadence, Watch};
use crate::udp::rtp::RTP_HEADER_LEN;
pub(crate) const SHARDS: usize = 8;

pub(crate) struct Receivers {
    peers: Vec<SocketAddr>,
    connections: usize,
    stops: Vec<mpsc::Sender<()>>,
    reports: Vec<mpsc::Receiver<Vec<Watch>>>,
}

impl Receivers {
    pub(crate) async fn start(connections: usize) -> Result<Self, std::io::Error> {
        let mut peers = Vec::with_capacity(SHARDS);
        let mut stops = Vec::with_capacity(SHARDS);
        let mut reports = Vec::with_capacity(SHARDS);
        for shard in 0..SHARDS {
            let socket = UdpSocket::bind(("127.0.0.1", 0)).await?;
            peers.push(socket.local_addr()?);
            let (stop, mut stopped) = mpsc::channel(1);
            let (report, arriving) = mpsc::channel(1);
            let mut watched: Vec<Watch> =
                (0..connections.div_ceil(SHARDS)).map(|_| Watch::default()).collect();
            tokio::spawn(async move {
                let mut packet = vec![0u8; 1600];
                loop {
                    let arrived = tokio::select! {
                        biased;
                        _ = stopped.recv() => break,
                        arrived = socket.recv_from(&mut packet) => arrived,
                    };
                    match arrived {
                        Ok((len, _from)) => {
                            let stream = ssrc(&packet[..len])
                                .and_then(|id| id.checked_sub(1))
                                .filter(|index| *index as usize % SHARDS == shard)
                                .map(|index| index as usize / SHARDS);
                            if let Some(slot) = stream.and_then(|index| watched.get_mut(index)) {
                                slot.record(Instant::now());
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = report.send(watched).await;
            });
            stops.push(stop);
            reports.push(arriving);
        }
        Ok(Self { peers, connections, stops, reports })
    }

    pub(crate) fn peer(&self, index: usize) -> SocketAddr {
        self.peers[index % SHARDS]
    }

    pub(crate) async fn finish(mut self) -> Vec<Cadence> {
        for stop in &self.stops {
            let _ = stop.send(()).await;
        }
        let mut every = vec![Cadence::default(); self.connections];
        for (shard, report) in self.reports.iter_mut().enumerate() {
            if let Some(watched) = report.recv().await {
                for (slot, watch) in watched.into_iter().enumerate() {
                    if let Some(entry) = every.get_mut(slot * SHARDS + shard) {
                        *entry = watch.cadence();
                    }
                }
            }
        }
        every
    }
}

fn ssrc(packet: &[u8]) -> Option<u32> {
    let four: [u8; 4] = packet.get(RTP_HEADER_LEN - 4..RTP_HEADER_LEN)?.try_into().ok()?;
    Some(u32::from_be_bytes(four))
}
