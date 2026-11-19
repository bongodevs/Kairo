use aes_gcm::{Aes256Gcm, KeyInit, aead::AeadInPlace};
use chacha20poly1305::XChaCha20Poly1305;

use crate::config::CipherPolicy;
use crate::udp::rtp::RTP_HEADER_LEN;
pub const XCHACHA_MODE: &str = "aead_xchacha20_poly1305_rtpsize";
pub(crate) const AES_MODE: &str = "aead_aes256_gcm_rtpsize";
pub const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const NONCE_PREFIX_LEN: usize = 4;

pub const MAX_PACKET_BYTES: usize = 1276;
const DAVE_MAX_OVERHEAD: usize = 75;

pub(crate) const MAX_DATAGRAM_BYTES: usize =
    MAX_PACKET_BYTES + DAVE_MAX_OVERHEAD + RTP_HEADER_LEN + TAG_LEN + NONCE_PREFIX_LEN;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("unsupported encryption mode: {0}")]
    UnsupportedMode(String),
    #[error("secret key must be {KEY_LEN} bytes, got {0}")]
    BadKeyLen(usize),
    #[error("no mutually supported encryption mode")]
    NoCommonMode,
    #[error("{0}")]
    Buffer(String),
    #[error("seal failed: {0}")]
    Seal(String),
}

pub struct TransportCipher {
    inner: Inner,
    seq: u32,
}

enum Inner {
    XChaCha(XChaCha20Poly1305),
    Aes(Box<Aes256Gcm>),
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn hardware_aes() -> bool {
    std::arch::is_x86_feature_detected!("aes")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn hardware_aes() -> bool {
    false
}

impl TransportCipher {
    pub(crate) fn select_mode(
        modes: &[String],
        policy: CipherPolicy,
    ) -> Result<&'static str, CryptoError> {
        let preference = match policy {
            CipherPolicy::Heuristic if hardware_aes() => [AES_MODE, XCHACHA_MODE],
            CipherPolicy::Heuristic | CipherPolicy::PreferXChaCha => [XCHACHA_MODE, AES_MODE],
            CipherPolicy::PreferAes => [AES_MODE, XCHACHA_MODE],
            CipherPolicy::ServerOrder => {
                return modes
                    .iter()
                    .find_map(|mode| match mode.as_str() {
                        AES_MODE => Some(AES_MODE),
                        XCHACHA_MODE => Some(XCHACHA_MODE),
                        _ => None,
                    })
                    .ok_or(CryptoError::NoCommonMode);
            }
        };
        preference
            .into_iter()
            .find(|wanted| modes.iter().any(|mode| mode == wanted))
            .ok_or(CryptoError::NoCommonMode)
    }

    pub fn new(mode: &str, secret_key: &[u8]) -> Result<Self, CryptoError> {
        if secret_key.len() != KEY_LEN {
            return Err(CryptoError::BadKeyLen(secret_key.len()));
        }
        Self::build(mode, secret_key)
    }

    fn build(mode: &str, secret_key: &[u8]) -> Result<Self, CryptoError> {
        let inner = match mode {
            XCHACHA_MODE => Inner::XChaCha(
                XChaCha20Poly1305::new_from_slice(secret_key)
                    .map_err(|_| CryptoError::BadKeyLen(secret_key.len()))?,
            ),
            AES_MODE => Inner::Aes(Box::new(
                Aes256Gcm::new_from_slice(secret_key)
                    .map_err(|_| CryptoError::BadKeyLen(secret_key.len()))?,
            )),
            other => return Err(CryptoError::UnsupportedMode(other.to_string())),
        };
        Ok(Self { inner, seq: getrandom::u32().map(|r| r % 418 + 1).unwrap_or(1) })
    }

    pub fn seal(
        &mut self,
        header: &[u8],
        payload: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        let need = header.len() + payload.len() + TAG_LEN + NONCE_PREFIX_LEN;
        if out.len() < need {
            return Err(CryptoError::Buffer(format!("output buffer too small: need {need}")));
        }
        out[..header.len()].copy_from_slice(header);
        out[header.len()..header.len() + payload.len()].copy_from_slice(payload);
        let body = &mut out[header.len()..header.len() + payload.len()];
        let nonce = self.seq.to_le_bytes();
        let tag = match &self.inner {
            Inner::XChaCha(cipher) => {
                let mut full = [0u8; 24];
                full[..NONCE_PREFIX_LEN].copy_from_slice(&nonce);
                cipher
                    .encrypt_in_place_detached(&full.into(), header, body)
                    .map_err(|err| CryptoError::Seal(err.to_string()))?
            }
            Inner::Aes(cipher) => {
                let mut full = [0u8; 12];
                full[..NONCE_PREFIX_LEN].copy_from_slice(&nonce);
                cipher
                    .encrypt_in_place_detached(&full.into(), header, body)
                    .map_err(|err| CryptoError::Seal(err.to_string()))?
            }
        };
        let body_end = header.len() + payload.len();
        out[body_end..body_end + TAG_LEN].copy_from_slice(tag.as_slice());
        out[body_end + TAG_LEN..body_end + TAG_LEN + NONCE_PREFIX_LEN].copy_from_slice(&nonce);
        self.seq = self.seq.wrapping_add(1);
        Ok(need)
    }
}

pub const fn sealed_len(header_len: usize, payload_len: usize) -> usize {
    header_len + payload_len + TAG_LEN + NONCE_PREFIX_LEN
}

const _: () = assert!(RTP_HEADER_LEN == 12);

#[cfg(test)]
#[path = "../../tests/crypto.rs"]
mod tests;
