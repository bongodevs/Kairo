use serde::Serialize;
use serde_json::json;

use super::error::ConnectionError;
use super::path::Path;
use crate::gateway::frame_text;
use crate::udp::rtp::OPUS_PAYLOAD_TYPE;
pub(super) const OP_SELECT_PROTOCOL: u8 = 1;
pub(super) const OP_SESSION_DESCRIPTION: u8 = 4;
pub(super) const OP_SPEAKING: u8 = 5;
pub(super) const OP_RESUMED: u8 = 9;
pub(super) const OP_VIDEO: u8 = 12;
pub(super) const OPUS_PRIORITY: u16 = 1000;

#[derive(Serialize)]
struct Codec {
    name: &'static str,
    payload_type: u16,
    priority: u16,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct ProtocolData {
    address: String,
    port: u16,
    mode: String,
}

#[derive(Serialize)]
struct SelectProtocol {
    protocol: &'static str,
    address: String,
    port: u16,
    mode: String,
    data: ProtocolData,
    codecs: [Codec; 1],
    rtc_connection_id: String,
}

#[derive(Serialize)]
pub(super) struct SpeakingUpdate {
    pub(super) speaking: u8,
    pub(super) delay: u16,
    pub(super) ssrc: u32,
}

pub(super) fn select_protocol(path: &Path) -> Result<String, ConnectionError> {
    let address = path.local.ip().to_string();
    let data =
        ProtocolData { address: address.clone(), port: path.local.port(), mode: path.mode.clone() };
    let announcement = SelectProtocol {
        protocol: "udp",
        address,
        port: path.local.port(),
        mode: path.mode.clone(),
        data,
        codecs: [Codec {
            name: "opus",
            payload_type: u16::from(OPUS_PAYLOAD_TYPE),
            priority: OPUS_PRIORITY,
            kind: "audio",
        }],
        rtc_connection_id: connection_id()?,
    };
    Ok(frame_text(OP_SELECT_PROTOCOL, &announcement)?)
}

pub(super) fn video_announcement(ssrc: u32) -> Result<String, ConnectionError> {
    Ok(frame_text(
        OP_VIDEO,
        &json!({
            "audio_ssrc": u64::from(ssrc),
            "video_ssrc": 0,
            "rtx_ssrc": 0,
        }),
    )?)
}

pub(super) fn initial_speaking(ssrc: u32) -> Result<String, ConnectionError> {
    Ok(frame_text(OP_SPEAKING, &SpeakingUpdate { speaking: 0, delay: 0, ssrc })?)
}

pub(super) fn connection_id() -> Result<String, ConnectionError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ConnectionError::Entropy)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok(format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]))
}
