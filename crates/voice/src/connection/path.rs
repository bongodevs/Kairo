use std::net::{IpAddr, SocketAddr};

use serde_json::Value;
use tokio::net::UdpSocket;

use super::error::ConnectionError;
use crate::config::VoiceConfig;
use crate::crypto::TransportCipher;
use crate::gateway::Ready;
use crate::udp;
pub(super) struct Path {
    pub(super) mode: String,
    pub(super) ssrc: u32,
    pub(super) udp: UdpSocket,
    pub(super) local: SocketAddr,
}

pub(super) async fn transport(
    ready: &Ready,
    config: &VoiceConfig,
) -> Result<Path, ConnectionError> {
    let mode = TransportCipher::select_mode(&ready.modes, config.cipher_policy)
        .map(str::to_owned)
        .map_err(|_| ConnectionError::NoMode)?;
    let server = voice_server(ready)?;
    let (udp, local) = udp::bind_and_discover(server, ready.ssrc, config.high_packet_priority)
        .await
        .map_err(|err| ConnectionError::Discovery(err.to_string()))?;
    Ok(Path { mode, ssrc: ready.ssrc, udp, local })
}

#[cfg(test)]
pub(super) fn negotiated_mode(ready: &Ready) -> Result<String, ConnectionError> {
    TransportCipher::select_mode(&ready.modes, crate::config::CipherPolicy::Heuristic)
        .map(str::to_owned)
        .map_err(|_| ConnectionError::NoMode)
}

pub(super) fn voice_server(ready: &Ready) -> Result<SocketAddr, ConnectionError> {
    if let Ok(address) = ready.ip.parse::<SocketAddr>() {
        return Ok(address);
    }
    let address =
        ready.ip.parse::<IpAddr>().map_err(|_| ConnectionError::Endpoint(ready.ip.clone()))?;
    Ok(SocketAddr::new(address, ready.port))
}

pub(super) fn key_bytes(data: &Value) -> Result<Vec<u8>, ConnectionError> {
    let key = data.get("secret_key").and_then(Value::as_array).ok_or_else(|| {
        ConnectionError::SessionDescription("secret_key is not a byte array".into())
    })?;
    key.iter()
        .map(|byte| {
            byte.as_u64().and_then(|value| u8::try_from(value).ok()).ok_or_else(|| {
                ConnectionError::SessionDescription("secret_key byte out of range".into())
            })
        })
        .collect()
}
