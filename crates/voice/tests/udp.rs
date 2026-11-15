use super::*;

#[test]
fn request_has_the_discord_shape() {
    let packet = build_request(0x0102_0304);
    assert_eq!(&packet[0..4], &[0, 1, 0, 70]);
    assert_eq!(&packet[4..8], &[1, 2, 3, 4]);
    assert!(packet[8..].iter().all(|byte| *byte == 0));
}

fn response(ssrc: u32, ip: &str, port: u16) -> Vec<u8> {
    let mut packet = build_request(ssrc);
    packet[0..2].copy_from_slice(&RESPONSE_TYPE.to_be_bytes());
    packet[8..8 + ip.len()].copy_from_slice(ip.as_bytes());
    packet[72..74].copy_from_slice(&port.to_be_bytes());
    packet.to_vec()
}

#[test]
fn response_parses_ip_and_port() {
    let (got_ssrc, ip, port) = parse_response(&response(42, "123.45.67.89", 50000)).unwrap();
    assert_eq!((got_ssrc, ip, port), (42, "123.45.67.89".parse().unwrap(), 50000));
}

#[test]
fn bad_responses_are_malformed() {
    assert!(matches!(parse_response(&response(1, "999.1.1.1", 1)), Err(UdpError::Malformed(_))));
    assert!(matches!(parse_response(&response(1, "not-an-ip", 1)), Err(UdpError::Malformed(_))));
    assert!(matches!(parse_response(&[0u8; 10]), Err(UdpError::Malformed(_))));
    let mut wrong_type = response(1, "1.2.3.4", 5);
    wrong_type[1] = 7;
    assert!(matches!(parse_response(&wrong_type), Err(UdpError::Malformed(_))));
}

async fn fake_voice_server() -> SocketAddr {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        let mut packet = [0u8; PACKET_LEN];
        loop {
            let (read, peer) = server.recv_from(&mut packet).await.unwrap();
            let ssrc = u32::from_be_bytes(packet[4..8].try_into().unwrap());
            let mut reply = build_request(ssrc);
            reply[0..2].copy_from_slice(&RESPONSE_TYPE.to_be_bytes());
            reply[8..17].copy_from_slice(b"127.0.0.1");
            reply[72..74].copy_from_slice(&peer.port().to_be_bytes());
            server.send_to(&reply[..read.max(PACKET_LEN)], peer).await.unwrap();
        }
    });
    addr
}

#[tokio::test]
async fn discovery_returns_the_external_address() {
    let server = fake_voice_server().await;
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(server).await.unwrap();
    let ours = socket.local_addr().unwrap();

    let discovered = discover_local(&socket, 1234).await.unwrap();
    assert_eq!(discovered.ip(), ours.ip());
    assert_eq!(discovered.port(), ours.port());
}

#[tokio::test(start_paused = true)]
async fn discovery_gives_up_after_ten_retries() {
    let quiet = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(quiet.local_addr().unwrap()).await.unwrap();
    let started = tokio::time::Instant::now();
    let err = discover_local(&socket, 7).await.expect_err("no reply");
    assert!(matches!(err, UdpError::Timeout), "{err}");
    assert_eq!(started.elapsed(), RETRY_PERIOD * MAX_ATTEMPTS);
}

#[cfg(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "cygwin",
))]
#[tokio::test]
async fn high_priority_sets_the_low_delay_and_throughput_bits() {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    set_high_priority(&socket, false).expect("set IP_TOS");
    assert_eq!(socket.tos_v4().unwrap() & 0x18, 0x18);
}
