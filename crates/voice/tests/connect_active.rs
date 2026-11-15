use super::*;

#[tokio::test(start_paused = true)]
async fn an_active_dry_source_stays_armed_for_a_later_frame() {
    let voice = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (mut connection, inbound) = driven(voice.local_addr().unwrap()).await;
    inbound.send(text(&session_description(0, &[0x5au8; KEY_LEN], XCHACHA_MODE))).await.unwrap();
    tokio::task::yield_now().await;

    let frames = connection.frames();
    let mut source = Scripted::new([None, Some(vec![0xa1; 4])]).active();
    let driver = tokio::spawn(async move {
        let code = connection.run_with(&mut source).await.expect("pump runs").code;
        (code, source.pulled)
    });
    frames.notify().await.expect("the pump is live");
    pace(2).await;

    assert_eq!(read_datagrams(&voice).len(), 1, "the later frame is sent");
    drop(inbound);
    assert_eq!(driver.await.expect("driver finishes"), (1000, 2));
}
