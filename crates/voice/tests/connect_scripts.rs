//! The parts more than one of these tests needs: a `Redial` that hands out scripted gateways, and
//! the plumbing for a run measured across many connections at once.

use super::*;

/// The timestamp and ssrc of every datagram, in the order they arrived.
pub(super) fn stream_of(packets: &[Vec<u8>]) -> Vec<(u32, u32)> {
    packets
        .iter()
        .map(|packet| {
            let (_seq, timestamp, _len, _nonce, ssrc) = rtp_shape(packet);
            (timestamp, ssrc)
        })
        .collect()
}
/// at the same voice server. `dialled` says when a re-dial is live, because the test cannot see
/// a socket the pump has already taken over, and `intros` records what was asked for.
pub(super) struct Reconnects {
    pub(super) server: SocketAddr,
    pub(super) next: VecDeque<(u32, bool, Vec<Inbound>, mpsc::Receiver<Inbound>)>,
    pub(super) dialled: mpsc::Sender<u32>,
    pub(super) intros: Vec<Intro>,
}

impl Redial<LiveSocket> for Reconnects {
    async fn redial(
        &mut self,
        target: &VoiceTarget,
        intro: Intro,
    ) -> Result<Reopened<LiveSocket>, ConnectionError> {
        self.intros.push(intro);
        let (ssrc, resumes, frames, queue) =
            self.next.pop_front().expect("the script covers every re-dial");
        let mut opening = match intro {
            Intro::Resume(_) if resumes => {
                vec![text(r#"{"op":8,"d":{"heartbeat_interval":45000}}"#), resumed()]
            }
            Intro::Resume(_) => vec![text(r#"{"op":8,"d":{"heartbeat_interval":45000}}"#)],
            Intro::Identify => gateway_opening(ssrc),
        };
        opening.extend(frames);
        self.dialled.send(ssrc).await.expect("the test is watching for re-dials");
        let gateway = Gateway::establish(
            LiveSocket::new(opening, queue),
            target,
            intro,
            VoiceConfig::default(),
        )
        .await?;
        match intro {
            Intro::Resume(_) => Ok(Reopened::Resumed(gateway)),
            Intro::Identify => {
                let udp = UdpSocket::bind(("127.0.0.1", 0)).await.expect("a media socket");
                udp.connect(self.server).await.expect("a voice server");
                let local = udp.local_addr().expect("a bound socket");
                Ok(Reopened::Ready(gateway, media_path(udp, local, ssrc)))
            }
        }
    }
}
/// Every connection's first frame notice, all at once: a thousand sequential waits on a runtime
/// that is busy waking a thousand pumps would take longer than the run it is measuring.
pub(super) fn notify(
    handles: Vec<crate::connection::Frames>,
) -> tokio::task::JoinSet<Result<(), ConnectionError>> {
    let mut calls = tokio::task::JoinSet::new();
    for handle in handles {
        calls.spawn(async move { handle.notify().await });
    }
    calls
}

/// Every connection's hang-up, all at once.
pub(super) fn disconnect(
    handles: Vec<crate::connection::Lifecycle>,
) -> tokio::task::JoinSet<Result<(), ConnectionError>> {
    let mut calls = tokio::task::JoinSet::new();
    for handle in handles {
        calls.spawn(async move { handle.disconnect().await });
    }
    calls
}

/// Finish every queued call and say each one worked.
pub(super) async fn all(mut calls: tokio::task::JoinSet<Result<(), ConnectionError>>, what: &str) {
    while let Some(done) = calls.join_next().await {
        done.expect("the harness task runs").expect(what);
    }
}

/// What every pump wrote, once they have all stopped.
pub(super) async fn stop(handles: Vec<tokio::task::JoinHandle<FrameStats>>) -> (u64, u64) {
    let mut sent = 0;
    let mut skipped = 0;
    for handle in handles {
        let stats = handle.await.expect("every pump stops on request");
        sent += stats.sent;
        skipped += stats.skipped;
    }
    (sent, skipped)
}
