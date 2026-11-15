use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherPolicy {
    Heuristic,
    ServerOrder,
    PreferAes,
    PreferXChaCha,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceConfig {
    pub gateway_connect_timeout: Duration,
    pub send_speaking_stop: bool,
    pub high_packet_priority: bool,
    pub deafened: bool,
    pub wss_port_override: bool,
    pub cipher_policy: CipherPolicy,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            gateway_connect_timeout: Duration::from_secs(10),
            send_speaking_stop: false,
            high_packet_priority: true,
            deafened: false,
            wss_port_override: false,
            cipher_policy: CipherPolicy::Heuristic,
        }
    }
}

#[cfg(test)]
#[path = "../tests/config.rs"]
mod tests;
