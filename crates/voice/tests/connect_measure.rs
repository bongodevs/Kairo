use super::*;
#[tokio::test]
async fn the_schedule_is_twenty_milliseconds_of_real_time() {
    // Not paused time: the one proof that the armed deadline is a real clock. The bounds are
    // wide because a debug build on a loaded machine jitters; drift would not stay inside them.
    const FRAMES: usize = 10;
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (mut connection, inbound) = driven(server).await;
    inbound.send(text(&session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE))).await.unwrap();
    tokio::task::yield_now().await;
    let frames = connection.frames();
    let mut source = Scripted::new((0..FRAMES).map(|n| Some(vec![0xa1u8; 8 + n])));
    let driver = tokio::spawn(async move {
        let code = connection.run_with(&mut source).await.expect("pump runs").code;
        (code, connection.frame_stats())
    });
    frames.notify().await.expect("the pump is live");

    let started = std::time::Instant::now();
    let mut arrivals = Vec::with_capacity(FRAMES);
    let mut buffer = vec![0u8; MAX_PACKET_BYTES];
    while arrivals.len() < FRAMES {
        let (len, _from) = voice.recv_from(&mut buffer).await.unwrap();
        assert_eq!(rtp_shape(&buffer[..len]).2, 8 + arrivals.len());
        arrivals.push(started.elapsed());
        assert!(started.elapsed() < Duration::from_secs(5), "the sender stalled: {arrivals:?}");
    }

    let gaps: Vec<Duration> =
        arrivals.windows(2).map(|pair| pair[1].saturating_sub(pair[0])).collect();
    // Only the upper bound belongs here: an absolute deadline that lands a frame late is chased
    // back on the next tick, so a short gap after a long one is the schedule holding its grid,
    // and a machine under load produces those slips by the millisecond. The cadence itself
    // (absolute deadlines, catch-up within three frames, no drift) is proven on a paused clock.
    for gap in &gaps {
        assert!(*gap <= Duration::from_millis(120), "a {gap:?} gap is a stall: {gaps:?}");
    }
    assert!(
        arrivals[FRAMES - 1] <= Duration::from_millis(20 * FRAMES as u64 + 300),
        "no drift over the whole run: {:?}",
        arrivals[FRAMES - 1]
    );
    drop(inbound);
    let (code, stats) = driver.await.expect("the driver finishes");
    assert_eq!(code, 1000);
    assert_eq!(stats.sent, FRAMES as u64, "every scheduled frame went out on the real clock");
}

/// V13's measurement of the production driver: `connections` connections, each with a
/// negotiated key, a UDP path of its own and a source that never runs dry, all armed in the
/// same instant, which is the worst shape a node can hand a scheduler. `#[ignore]`d because it
/// spends seconds of real time and a thousand sockets.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "a measurement over seconds of real time and a thousand sockets, not a gate"]
async fn a_thousand_connections_hold_their_pace() {
    println!(
        "one sender holds {} B of frame buffer and {} B of datagram buffers in a {} B Media",
        MAX_PACKET_BYTES,
        2 * MAX_DATAGRAM_BYTES,
        std::mem::size_of::<Media>()
    );
    for connections in [1usize, 100, 1000] {
        let run = paced_run(connections, Duration::from_secs(3)).await;
        println!("{}", run.line());
        assert!(
            run.delivered() >= scale::KEEP,
            "the pump delivered {} % of the nominal rate at {connections} connections, below \
             the bar the pacing model is judged at",
            run.delivered(),
        );
    }
}

/// Set up `connections` paced connections, let every one of them get its key, arm them
/// together, and run for `window`.
async fn paced_run(connections: usize, window: Duration) -> scale::Outcome {
    let sinks = scale::Receivers::start(connections).await.expect("media sinks");
    let memory = scale::Cost::now();
    let built = std::time::Instant::now();
    let mut running = Vec::with_capacity(connections);
    let mut armed = Vec::with_capacity(connections);
    let mut hung_up = Vec::with_capacity(connections);
    // The queue ends of the mock gateways: dropping them would tell a pump the peer hung up.
    let mut gateways = Vec::with_capacity(connections);
    for index in 0..connections {
        let ssrc = index as u32 + 1;
        let (sender, inbound) = mpsc::channel(1);
        let mut opening = gateway_opening(ssrc);
        opening.push(text(&session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE)));
        let gateway = Gateway::establish(
            LiveSocket::new(opening, inbound),
            &target(),
            Intro::Identify,
            VoiceConfig::default(),
        )
        .await
        .expect("handshake");
        let udp = UdpSocket::bind(("127.0.0.1", 0)).await.expect("a media socket");
        udp.connect(sinks.peer(index)).await.expect("a sink");
        let local = udp.local_addr().expect("a bound socket");
        let mut connection = Connection::attach(
            gateway,
            &target(),
            media_path(udp, local, ssrc),
            VoiceConfig::default(),
        )
        .await
        .expect("attach");
        armed.push(connection.frames());
        hung_up.push(connection.lifecycle());
        gateways.push(sender);
        running.push(tokio::spawn(async move {
            let mut source = scale::Speech;
            let _ = connection.run_with(&mut source).await;
            connection.frame_stats()
        }));
    }
    let setup = built.elapsed();
    // Every pump has to take its session description before the pace starts, or the shut gate
    // eats a frame per connection and the run measures the harness instead of the schedule.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let armed_too = std::time::Instant::now();
    all(notify(armed), "the pump takes the notice").await;
    let arming = armed_too.elapsed();
    let cpu = scale::Cost::now();
    let started = std::time::Instant::now();
    tokio::time::sleep(window).await;
    let paced = started.elapsed();
    let hung_up_too = std::time::Instant::now();
    all(disconnect(hung_up), "the pump is still taking commands").await;
    let (sent, skipped) = stop(running).await;
    let hanging_up = hung_up_too.elapsed();
    drop(gateways);
    let (cadence, quietest) = scale::spread(&sinks.finish().await);
    println!("harness: {setup:?} building, {arming:?} arming, {hanging_up:?} hanging up");
    scale::Outcome {
        what: "the production pump",
        connections,
        window: paced,
        sent,
        skipped,
        cadence,
        quietest,
        cpu,
        memory,
    }
}
