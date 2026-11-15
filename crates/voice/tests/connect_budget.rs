use super::*;
/// The server forgot us, so a resume would only be refused again. Every new READY hands back a
/// session worth losing, so the whole budget goes on fresh identifies before the app is told the
/// session is gone, which only a new voice update can put right.
#[tokio::test(start_paused = true)]
async fn a_forgotten_session_identifies_until_the_budget_is_spent() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let (dialled, mut saw) = mpsc::channel(4);
    let lost = || Inbound::Closed(4006, "the session is no longer valid".to_owned());
    let queues = (0..3).map(|_| mpsc::channel(8)).collect::<Vec<_>>();
    let mut network = Reconnects {
        server,
        next: queues
            .into_iter()
            .map(|(_sent, queue)| queue)
            .zip(41..44)
            .map(|(queue, ssrc)| (ssrc, false, vec![lost()], queue))
            .collect(),
        dialled,
        intros: Vec::new(),
    };
    let (mut connection, gateway) = driven(server).await;
    gateway.send(lost()).await.expect("the pump is live");
    let driver = tokio::spawn(async move {
        let closing = connection
            .pump_retrying(&mut NoFrames, &mut network)
            .await
            .expect("the supervisor runs");
        (closing, network.intros)
    });
    for _ in 0..3 {
        saw.recv().await.expect("a fresh identify was sent");
    }
    let (closing, intros) = driver.await.expect("the supervisor finishes");
    assert_eq!(intros, [Intro::Identify; 3], "a session the server forgot is never resumed");
    assert_eq!(closing.code, 4006);
    assert!(closing.session_lost(), "{closing:?}");
}

/// Three failed resume handshakes are the whole budget for one outage. Buffered peer events buy
/// nothing; only an op-9 acknowledgment proves the session and pays the attempts back.
#[tokio::test(start_paused = true)]
async fn three_re_opened_gateways_are_the_whole_budget_for_one_outage() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let died = || Inbound::Closed(4015, "the voice host died".to_owned());
    let (dialled, mut saw) = mpsc::channel(4);
    let queues = (0..3).map(|_| mpsc::channel(8)).collect::<Vec<_>>();
    let mut network = Reconnects {
        server,
        next: queues
            .into_iter()
            .map(|(_sent, queue)| queue)
            .zip(41..44)
            .map(|(queue, ssrc)| (ssrc, false, vec![peer_left(), peer_left(), died()], queue))
            .collect(),
        dialled,
        intros: Vec::new(),
    };
    let (mut connection, gateway) = driven(server).await;
    gateway.send(died()).await.expect("the pump is live");
    let driver = tokio::spawn(async move {
        let closing = connection
            .pump_retrying(&mut NoFrames, &mut network)
            .await
            .expect("the supervisor runs");
        (closing, network.intros)
    });
    for _ in 0..3 {
        saw.recv().await.expect("a gateway was dialled");
    }
    let (closing, intros) = driver.await.expect("the supervisor finishes");
    assert_eq!(
        intros,
        [Intro::Resume(1), Intro::Resume(1), Intro::Resume(1)],
        "each attempt spends one gateway, and READY's seq is where each one picks up"
    );
    assert_eq!(
        closing,
        Disconnect::remote(4015, "the voice host died"),
        "the last pre-resume close is reported with its wire code"
    );
    assert!(!closing.session_lost(), "the session is fine, the host is not");
}

/// A resume the server agreed to is our own session proving itself, so the attempts the outage
/// cost are owed back: the fourth gateway gets dialled and the run ends on the host's clean
/// close, not on the budget. Three re-dials would be the whole budget, spent and unreturned.
#[tokio::test(start_paused = true)]
async fn a_resumed_session_pays_the_retry_budget_back() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = voice.local_addr().unwrap();
    let died = || Inbound::Closed(4015, "the voice host died".to_owned());
    let (dialled, mut saw) = mpsc::channel(4);
    let queues = (0..4).map(|_| mpsc::channel(8)).collect::<Vec<_>>();
    let mut network = Reconnects {
        server,
        next: queues
            .into_iter()
            .map(|(_sent, queue)| queue)
            .zip(41..45)
            .enumerate()
            .map(|(index, (queue, ssrc))| {
                let closing = if index == 3 {
                    Inbound::Closed(1000, "the run ended".to_owned())
                } else {
                    died()
                };
                (ssrc, true, vec![closing], queue)
            })
            .collect(),
        dialled,
        intros: Vec::new(),
    };
    let (mut connection, gateway) = driven(server).await;
    gateway.send(died()).await.expect("the pump is live");
    let driver = tokio::spawn(async move {
        let closing = connection
            .pump_retrying(&mut NoFrames, &mut network)
            .await
            .expect("the supervisor runs");
        (closing, network.intros)
    });
    for _ in 0..4 {
        saw.recv().await.expect("a gateway was dialled");
    }
    let (closing, intros) = driver.await.expect("the supervisor finishes");
    assert_eq!(
        intros,
        [Intro::Resume(1); 4],
        "the fourth attempt only happens because a resume returned the three the budget holds"
    );
    assert_eq!(closing.code, 1000, "{closing:?}");
}
