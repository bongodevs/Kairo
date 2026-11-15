//! UDP transport: Discord's IP discovery (the 74-byte holepunch packet).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
const REQUEST_TYPE: u16 = 1;
const RESPONSE_TYPE: u16 = 2;
const BODY_LEN: u16 = 70;
const PACKET_LEN: usize = (BODY_LEN + 4) as usize;
const RETRY_PERIOD: Duration = Duration::from_secs(1);
const MAX_ATTEMPTS: u32 = 10;

#[derive(Debug, thiserror::Error)]
pub(crate) enum UdpError {
    #[error("io: {0}")]
    Io(String),
    #[error("no valid discovery response after 10 attempts")]
    Timeout,
    #[error("malformed discovery response: {0}")]
    Malformed(String),
}

fn build_request(ssrc: u32) -> [u8; PACKET_LEN] {
    let mut packet = [0u8; PACKET_LEN];
    packet[0..2].copy_from_slice(&REQUEST_TYPE.to_be_bytes());
    packet[2..4].copy_from_slice(&BODY_LEN.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    packet
}

fn parse_response(packet: &[u8]) -> Result<(u32, IpAddr, u16), UdpError> {
    if packet.len() < PACKET_LEN {
        return Err(UdpError::Malformed(format!("{} bytes, need {PACKET_LEN}", packet.len())));
    }
    let kind = u16::from_be_bytes(packet[0..2].try_into().unwrap_or([0; 2]));
    if kind != RESPONSE_TYPE {
        return Err(UdpError::Malformed(format!("type {kind}")));
    }
    let ssrc = u32::from_be_bytes(packet[4..8].try_into().unwrap_or([0; 4]));
    let field = &packet[8..72];
    let end = field.iter().position(|byte| *byte == 0).unwrap_or(field.len());
    let text =
        std::str::from_utf8(&field[..end]).map_err(|err| UdpError::Malformed(err.to_string()))?;
    let ip =
        text.parse::<IpAddr>().map_err(|err| UdpError::Malformed(format!("ip {text:?}: {err}")))?;
    let port = u16::from_be_bytes(packet[72..74].try_into().unwrap_or([0; 2]));
    Ok((ssrc, ip, port))
}

pub(crate) async fn discover_local(socket: &UdpSocket, ssrc: u32) -> Result<SocketAddr, UdpError> {
    let request = build_request(ssrc);
    let mut packet = [0u8; PACKET_LEN];
    for _ in 0..MAX_ATTEMPTS {
        socket.send(&request).await.map_err(|err| UdpError::Io(err.to_string()))?;
        if let Ok(Ok(read)) = tokio::time::timeout(RETRY_PERIOD, socket.recv(&mut packet)).await
            && let Ok((got_ssrc, ip, port)) = parse_response(&packet[..read])
            && got_ssrc == ssrc
        {
            return Ok(SocketAddr::new(ip, port));
        }
    }
    Err(UdpError::Timeout)
}

pub(crate) async fn bind_and_discover(
    voice_server: SocketAddr,
    ssrc: u32,
    high_priority: bool,
) -> Result<(UdpSocket, SocketAddr), UdpError> {
    let wildcard: IpAddr = if voice_server.is_ipv4() {
        Ipv4Addr::UNSPECIFIED.into()
    } else {
        Ipv6Addr::UNSPECIFIED.into()
    };
    let socket =
        UdpSocket::bind((wildcard, 0)).await.map_err(|err| UdpError::Io(err.to_string()))?;
    if high_priority {
        set_high_priority(&socket, voice_server.is_ipv6())
            .map_err(|err| UdpError::Io(err.to_string()))?;
    }
    socket.connect(voice_server).await.map_err(|err| UdpError::Io(err.to_string()))?;
    let discovered = discover_local(&socket, ssrc).await?;
    Ok((socket, discovered))
}

fn set_high_priority(socket: &UdpSocket, ipv6: bool) -> std::io::Result<()> {
    const LOW_DELAY_AND_THROUGHPUT: u32 = 0x10 | 0x08;
    if ipv6 {
        set_priority_v6(socket, LOW_DELAY_AND_THROUGHPUT)
    } else {
        set_priority_v4(socket, LOW_DELAY_AND_THROUGHPUT)
    }
}

#[cfg(not(any(
    target_os = "fuchsia",
    target_os = "redox",
    target_os = "solaris",
    target_os = "illumos",
    target_os = "haiku",
    target_os = "wasi",
)))]
fn set_priority_v4(socket: &UdpSocket, value: u32) -> std::io::Result<()> {
    socket.set_tos_v4(value)
}

#[cfg(any(
    target_os = "fuchsia",
    target_os = "redox",
    target_os = "solaris",
    target_os = "illumos",
    target_os = "haiku",
    target_os = "wasi",
))]
fn set_priority_v4(_socket: &UdpSocket, _value: u32) -> std::io::Result<()> {
    Ok(())
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
fn set_priority_v6(socket: &UdpSocket, value: u32) -> std::io::Result<()> {
    socket.set_tclass_v6(value)
}

#[cfg(not(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "cygwin",
)))]
fn set_priority_v6(_socket: &UdpSocket, _value: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/udp.rs"]
mod tests;
