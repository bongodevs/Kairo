use tokio::net::UdpSocket;

use crate::connection::{ConnectionError, Session};
use crate::crypto::sealed_len;
use crate::sender::{FrameSource, Media, SILENCE_FRAME};
use crate::udp::rtp::RTP_HEADER_LEN;

impl Session {
    pub(crate) fn tick<S: FrameSource>(
        &mut self,
        source: &mut S,
        udp: &UdpSocket,
        media: &mut Media,
    ) -> Result<(bool, bool), ConnectionError> {
        if !self.ready_to_send() {
            return Ok((true, false));
        }
        let Media { frame, encrypted, sealed, rtp, pacing } = media;
        let (raw, silence) = match source.provide(frame) {
            Some(len) if len <= frame.len() => {
                pacing.produced();
                (&frame[..len], false)
            }
            Some(_) => {
                pacing.skip();
                return Ok((true, false));
            }
            None if pacing.take_tail() => (&SILENCE_FRAME[..], true),
            None if source.is_active() => return Ok((true, false)),
            None => return Ok((false, false)),
        };
        let Session { cipher, dave, .. } = self;
        let Some(cipher) = cipher.as_mut() else {
            return Ok((true, false));
        };
        let payload = if dave.is_active() && !silence {
            if dave.encrypt_opus_into(raw, encrypted).is_none() {
                pacing.skip();
                return Ok((true, false));
            }
            &encrypted[..]
        } else {
            raw
        };
        let mut header = [0u8; RTP_HEADER_LEN];
        rtp.next_header(&mut header, false);
        let need = sealed_len(RTP_HEADER_LEN, payload.len());
        sealed.resize(need, 0);
        let Ok(written) = cipher.seal(&header, payload, sealed) else {
            pacing.skip();
            return Ok((true, false));
        };

        match udp.try_send(&sealed[..written]) {
            Ok(_) => pacing.written(),
            Err(_) => pacing.skip(),
        }
        Ok((true, !silence))
    }
}
