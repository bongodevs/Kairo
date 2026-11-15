use super::*;

#[test]
fn defaults_match_the_voice_client_policy() {
    let config = VoiceConfig::default();
    assert_eq!(config.gateway_connect_timeout, Duration::from_secs(10));
    assert!(!config.send_speaking_stop);
    assert!(config.high_packet_priority);
    assert!(!config.deafened);
    assert!(!config.wss_port_override);
    assert_eq!(config.cipher_policy, CipherPolicy::Heuristic);
}
