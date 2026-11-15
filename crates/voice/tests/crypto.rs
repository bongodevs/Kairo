use super::*;

fn key() -> [u8; KEY_LEN] {
    [7u8; KEY_LEN]
}

fn header() -> [u8; RTP_HEADER_LEN] {
    let mut h = [0u8; RTP_HEADER_LEN];
    h[0] = 0x80;
    h[1] = 120;
    h
}

fn cipher(mode: &str) -> TransportCipher {
    TransportCipher::new(mode, &key()).unwrap()
}

#[test]
fn select_prefers_aes_only_with_hardware() {
    let modes: Vec<String> = [XCHACHA_MODE, AES_MODE].iter().map(|m| m.to_string()).collect();
    if hardware_aes() {
        assert_eq!(
            TransportCipher::select_mode(&modes, CipherPolicy::Heuristic).unwrap(),
            AES_MODE
        );
    } else {
        assert_eq!(
            TransportCipher::select_mode(&modes, CipherPolicy::Heuristic).unwrap(),
            XCHACHA_MODE,
            "no AES instructions, so XChaCha20-Poly1305"
        );
    }
}

#[test]
fn select_falls_back_when_only_one_offered() {
    let only_xchacha = vec![XCHACHA_MODE.to_string()];
    assert_eq!(
        TransportCipher::select_mode(&only_xchacha, CipherPolicy::Heuristic).unwrap(),
        XCHACHA_MODE
    );
    let only_aes = vec![AES_MODE.to_string()];
    assert_eq!(TransportCipher::select_mode(&only_aes, CipherPolicy::Heuristic).unwrap(), AES_MODE);
}

#[test]
fn select_rejects_unsupported_modes_and_bad_keys() {
    let legacy = vec!["xsalsa20_poly1305".to_string()];
    assert!(matches!(
        TransportCipher::select_mode(&legacy, CipherPolicy::Heuristic),
        Err(CryptoError::NoCommonMode)
    ));
    assert!(matches!(
        TransportCipher::new(XCHACHA_MODE, &[0u8; 16]),
        Err(CryptoError::BadKeyLen(16))
    ));
}

#[test]
fn explicit_cipher_policies_override_the_heuristic() {
    let server_order = vec![XCHACHA_MODE.to_owned(), AES_MODE.to_owned()];
    assert_eq!(
        TransportCipher::select_mode(&server_order, CipherPolicy::ServerOrder).unwrap(),
        XCHACHA_MODE
    );
    assert_eq!(
        TransportCipher::select_mode(&server_order, CipherPolicy::PreferAes).unwrap(),
        AES_MODE
    );
}

#[test]
fn seal_lays_out_header_ct_tag_and_nonce() {
    let mut cipher = cipher(XCHACHA_MODE);
    cipher.seq = 5;
    let h = header();
    let payload = [0xABu8; 40];
    let mut out = vec![0u8; sealed_len(h.len(), payload.len()) + 8];
    let n = cipher.seal(&h, &payload, &mut out).unwrap();
    assert_eq!(n, sealed_len(h.len(), payload.len()));
    assert_eq!(&out[..12], &h[..]);
    assert_eq!(&out[n - NONCE_PREFIX_LEN..n], &5u32.to_le_bytes());
    assert_ne!(&out[12..12 + payload.len()], &payload);
    assert_eq!(cipher.seq, 6, "counter advances");
}

#[test]
fn seal_counters_are_little_endian_and_unique() {
    for mode in [XCHACHA_MODE, AES_MODE] {
        let mut cipher = cipher(mode);
        cipher.seq = 0x0102_0304;
        let h = header();
        let payload = [0x01u8; 8];
        let mut out = vec![0u8; sealed_len(h.len(), payload.len())];
        cipher.seal(&h, &payload, &mut out).unwrap();
        assert_eq!(&out[out.len() - 4..], &0x0102_0304u32.to_le_bytes(), "{mode} nonce prefix");
    }
}

#[test]
fn seal_rejects_short_output() {
    let mut cipher = cipher(XCHACHA_MODE);
    let h = header();
    let mut out = [0u8; 12];
    assert!(matches!(cipher.seal(&h, &[0u8; 32], &mut out), Err(CryptoError::Buffer(_))));
}
