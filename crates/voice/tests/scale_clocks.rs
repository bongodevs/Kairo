use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use super::FRAME;
use super::cost::{Cost, Outcome};
use super::gaps::{Cadence, Watch, spread};
use crate::sender::Media;
pub(crate) async fn per_connection_deadlines(
    slots: usize,
    window: Duration,
) -> Result<Outcome, tokio::task::JoinError> {
    let memory = Cost::now();
    let mut tasks = Vec::with_capacity(slots);
    for slot in 0..slots {
        tasks.push(tokio::spawn(ticking(Media::new(slot as u32 + 1), window)));
    }
    let opened = std::time::Instant::now();
    let cpu = Cost::now();
    let mut every = Vec::with_capacity(slots);
    for task in tasks {
        every.push(task.await?);
    }
    let (cadence, quietest) = spread(&every);
    let sent = cadence.frames;
    Ok(Outcome {
        what: "one deadline per slot",
        connections: slots,
        window: opened.elapsed(),
        sent,
        skipped: 0,
        cadence,
        quietest,
        cpu,
        memory,
    })
}

async fn ticking(mut media: Media, window: Duration) -> Cadence {
    let mut watch = Watch::default();
    let until = Instant::now() + window;
    media.pacing.arm();
    while let Some(due) = media.pacing.deadline() {
        tokio::time::sleep_until(due).await;
        watch.record(Instant::now());
        media.pacing.advance();
        if Instant::now() >= until {
            media.pacing.stop();
        }
    }
    watch.cadence()
}

pub(crate) async fn shared_pacer(
    slots: usize,
    window: Duration,
) -> Result<Outcome, tokio::task::JoinError> {
    let memory = Cost::now();
    let mut senders = Vec::with_capacity(slots);
    let mut tasks = Vec::with_capacity(slots);
    for slot in 0..slots {
        let (sender, tick) = mpsc::channel(1);
        senders.push(sender);
        tasks.push(tokio::spawn(listening(tick, Media::new(slot as u32 + 1))));
    }
    let opened = std::time::Instant::now();
    let cpu = Cost::now();
    let skipped = tokio::spawn(pacing(senders, window)).await?;
    let mut every = Vec::with_capacity(slots);
    for task in tasks {
        every.push(task.await?);
    }
    let (cadence, quietest) = spread(&every);
    let sent = cadence.frames;
    Ok(Outcome {
        what: "one pacer for all slots",
        connections: slots,
        window: opened.elapsed(),
        sent,
        skipped,
        cadence,
        quietest,
        cpu,
        memory,
    })
}

async fn pacing(slots: Vec<mpsc::Sender<()>>, window: Duration) -> u64 {
    let until = Instant::now() + window;
    let mut due = Instant::now();
    let mut dropped = 0;
    while Instant::now() < until {
        tokio::time::sleep_until(due).await;
        due += FRAME;
        for slot in &slots {
            dropped += u64::from(slot.try_send(()).is_err());
        }
    }
    dropped
}

async fn listening(mut tick: mpsc::Receiver<()>, media: Media) -> Cadence {
    let mut watch = Watch::default();
    while let Some(()) = tick.recv().await {
        watch.record(Instant::now());
    }
    drop(media);
    watch.cadence()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scale::KEEP;

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "a measurement over seconds of real time, not a gate"]
    async fn the_two_clocks_are_priced_against_each_other() {
        for slots in [1usize, 100, 1000] {
            let window = Duration::from_secs(3);
            let alone = per_connection_deadlines(slots, window).await.expect("the slots finish");
            let shared = shared_pacer(slots, window).await.expect("the pacer and its slots finish");
            println!("{}", alone.line());
            println!("{}", shared.line());
            assert!(
                alone.delivered() >= KEEP,
                "one deadline per slot delivered {} % of the nominal rate at {slots} slots, below \
                 the bar the decision is made at",
                alone.delivered(),
            );
        }
    }
}
