use super::*;
use crate::audio::FRAME_SAMPLES;
use crate::decode::Millis;
const PLAYERS: usize = 1_000;

pub(crate) fn rss() -> u64 {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let Ok(mut system) = sysinfo::System::new() else {
        return 0;
    };
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map_or(0, sysinfo::Process::memory)
}

#[test]
#[ignore = "fills 1000 default player buffers to measure resident memory"]
fn a_thousand_full_buffers_have_a_measured_bound() {
    let before = rss();
    let config = FrameBufferConfig::default();
    let buffers = full_buffers(config);
    let growth = rss().saturating_sub(before);
    let per_player = growth / PLAYERS as u64;
    eprintln!(
        "{PLAYERS} full buffers: {} MiB total, {} KiB/player ({} frames/player)",
        growth / (1024 * 1024),
        per_player / 1024,
        config.capacity()
    );
    assert!(per_player <= 384 * 1024, "a frame handoff exceeded its 384 KiB measured budget");
    drop(buffers);
    let retained_once = rss().saturating_sub(before);

    // glibc keeps its small-allocation arena resident. A second complete lifecycle must reuse it
    // rather than grow another arena's worth, which distinguishes caching from a live-object leak.
    let buffers = full_buffers(config);
    drop(buffers);
    let retained_twice = rss().saturating_sub(before);
    eprintln!(
        "after two create/drop cycles: {} MiB then {} MiB retained",
        retained_once / (1024 * 1024),
        retained_twice / (1024 * 1024)
    );
    assert!(
        retained_twice <= retained_once.saturating_add(16 * 1024 * 1024),
        "a second lifecycle grew RSS by more than 16 MiB"
    );
}

fn full_buffers(config: FrameBufferConfig) -> Vec<(FrameProducer, FrameConsumer)> {
    let mut buffers = Vec::with_capacity(PLAYERS);
    for player in 0..PLAYERS {
        let (mut producer, consumer) = frame_buffer(config);
        for frame_index in 0..config.capacity() {
            let packet = [u8::try_from(player % 251).expect("small"); 250];
            producer
                .blocking_send(
                    BufferedFrame::new(
                        &packet,
                        Millis::new(u64::try_from(frame_index).expect("small") * 20),
                        FRAME_SAMPLES,
                    )
                    .expect("frame"),
                )
                .expect("fill buffer");
        }
        buffers.push((producer, consumer));
    }
    buffers
}
