use alloc::vec::Vec;
use core::fmt;

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};

use super::{ProtoError, MAX_MESSAGE_LEN};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
pub const COUNTER_TRAILER_LEN: usize = 12;
pub const MIN_SEALED_LEN: usize = COUNTER_TRAILER_LEN + TAG_LEN + NONCE_LEN;
pub const MAX_PAYLOAD_LEN: usize = MAX_MESSAGE_LEN - MIN_SEALED_LEN;

#[derive(Clone)]
pub struct Crypto {
    cipher: XChaCha20Poly1305,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Opened {
    pub payload: Vec<u8>,
    pub reboot: u32,
    pub counter: u64,
}

impl fmt::Debug for Crypto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Crypto { key: <redacted> }")
    }
}

impl Crypto {
    pub fn new(shared_key: [u8; KEY_LEN]) -> Self {
        Self {
            cipher: XChaCha20Poly1305::new(&shared_key.into()),
        }
    }

    pub fn seal(
        &self,
        payload: &[u8],
        reboot: u32,
        counter: u64,
        nonce: &[u8; NONCE_LEN],
    ) -> Result<Vec<u8>, ProtoError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtoError::PlaintextTooLong {
                len: payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }
        let mut sealed = Vec::with_capacity(payload.len() + MIN_SEALED_LEN);
        sealed.extend_from_slice(payload);
        sealed.extend_from_slice(&reboot.to_le_bytes());
        sealed.extend_from_slice(&counter.to_le_bytes());
        let tag = self
            .cipher
            .encrypt_in_place_detached(XNonce::from_slice(nonce), &[], &mut sealed)
            .expect("a payload within MAX_PAYLOAD_LEN always seals");
        sealed.extend_from_slice(&tag);
        sealed.extend_from_slice(nonce);
        Ok(sealed)
    }

    pub fn open(&self, sealed: &[u8]) -> Result<Opened, ProtoError> {
        if sealed.len() < MIN_SEALED_LEN {
            return Err(ProtoError::SealedTooShort {
                len: sealed.len(),
                min: MIN_SEALED_LEN,
            });
        }
        let (ciphertext_and_tag, nonce) = sealed.split_at(sealed.len() - NONCE_LEN);
        let (ciphertext, tag) = ciphertext_and_tag.split_at(ciphertext_and_tag.len() - TAG_LEN);
        let mut plaintext = ciphertext.to_vec();
        self.cipher
            .decrypt_in_place_detached(
                XNonce::from_slice(nonce),
                &[],
                &mut plaintext,
                Tag::from_slice(tag),
            )
            .map_err(|_| ProtoError::Aead)?;
        let mut trailer = [0u8; COUNTER_TRAILER_LEN];
        trailer.copy_from_slice(&plaintext[plaintext.len() - COUNTER_TRAILER_LEN..]);
        plaintext.truncate(plaintext.len() - COUNTER_TRAILER_LEN);
        let [r0, r1, r2, r3, counter @ ..] = trailer;
        Ok(Opened {
            payload: plaintext,
            reboot: u32::from_le_bytes([r0, r1, r2, r3]),
            counter: u64::from_le_bytes(counter),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Frames, Reassembler};
    use alloc::format;
    use alloc::vec;
    use chacha20poly1305::aead::Aead;

    const KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
    const NONCE: [u8; NONCE_LEN] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
    ];

    fn crypto() -> Crypto {
        Crypto::new(KEY)
    }

    fn opened(payload: &[u8], reboot: u32, counter: u64) -> Opened {
        Opened {
            payload: payload.to_vec(),
            reboot,
            counter,
        }
    }

    #[test]
    fn seal_open_round_trips() {
        let c = crypto();
        let payload = b"\x01\x02\x03\x04 StartStopBolus";
        let sealed = c.seal(payload, 8, 1234, &NONCE).unwrap();
        assert_eq!(c.open(&sealed), Ok(opened(payload, 8, 1234)));
    }

    #[test]
    fn nonce_is_at_the_end_not_the_beginning() {
        let sealed = crypto().seal(b"abcdefgh", 0, 1, &NONCE).unwrap();
        assert_eq!(&sealed[sealed.len() - NONCE_LEN..], &NONCE);
        assert_ne!(&sealed[..NONCE_LEN], &NONCE);
    }

    #[test]
    fn wire_layout_is_ciphertext_then_tag_then_nonce() {
        let payload = b"0123456789";
        let sealed = crypto().seal(payload, 0, 1, &NONCE).unwrap();
        let reference = XChaCha20Poly1305::new(&KEY.into())
            .encrypt(
                XNonce::from_slice(&NONCE),
                &[payload.as_slice(), &[0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]].concat()[..],
            )
            .unwrap();
        assert_eq!(sealed.len(), payload.len() + MIN_SEALED_LEN);
        assert_eq!(&sealed[..sealed.len() - NONCE_LEN], reference.as_slice());
        assert_eq!(&sealed[sealed.len() - NONCE_LEN..], &NONCE);
    }

    #[test]
    fn empty_payload_still_carries_counters() {
        let c = crypto();
        let sealed = c.seal(&[], 3, 9, &NONCE).unwrap();
        assert_eq!(sealed.len(), MIN_SEALED_LEN);
        assert_eq!(c.open(&sealed), Ok(opened(&[], 3, 9)));
    }

    #[test]
    fn counter_trailer_is_little_endian_reboot_then_counter() {
        let sealed = crypto()
            .seal(b"XY", 0x1122_3344, 0x0102_0304_0506_0708, &NONCE)
            .unwrap();
        let plain = XChaCha20Poly1305::new(&KEY.into())
            .decrypt(
                XNonce::from_slice(&NONCE),
                &sealed[..sealed.len() - NONCE_LEN],
            )
            .unwrap();
        assert_eq!(&plain[..2], b"XY");
        assert_eq!(
            &plain[2..],
            &[0x44, 0x33, 0x22, 0x11, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn counter_trailer_is_stripped_from_the_returned_payload() {
        let c = crypto();
        let payload = vec![0xAAu8; 40];
        let sealed = c.seal(&payload, 7, 77, &NONCE).unwrap();
        assert_eq!(c.open(&sealed).unwrap().payload, payload);
    }

    #[test]
    fn counters_round_trip_at_their_extremes() {
        let c = crypto();
        for (reboot, counter) in [
            (0u32, 0u64),
            (1, 1),
            (u32::MAX, u64::MAX),
            (0, u64::MAX),
            (u32::MAX, 0),
            (8, 1_048_576),
        ] {
            let sealed = c.seal(b"payload", reboot, counter, &NONCE).unwrap();
            assert_eq!(c.open(&sealed), Ok(opened(b"payload", reboot, counter)));
        }
    }

    #[test]
    fn open_rejects_a_tampered_tag() {
        let c = crypto();
        let sealed = c.seal(b"bolus", 0, 1, &NONCE).unwrap();
        let tag_start = sealed.len() - NONCE_LEN - TAG_LEN;
        for i in tag_start..tag_start + TAG_LEN {
            let mut bad = sealed.clone();
            bad[i] ^= 0x01;
            assert_eq!(c.open(&bad), Err(ProtoError::Aead), "tag byte {i}");
        }
    }

    #[test]
    fn open_rejects_tampered_ciphertext_and_nonce() {
        let c = crypto();
        let sealed = c.seal(b"bolus", 0, 1, &NONCE).unwrap();

        let mut flipped_ciphertext = sealed.clone();
        flipped_ciphertext[0] ^= 0x01;
        assert_eq!(c.open(&flipped_ciphertext), Err(ProtoError::Aead));

        let mut flipped_nonce = sealed.clone();
        let last = flipped_nonce.len() - 1;
        flipped_nonce[last] ^= 0x01;
        assert_eq!(c.open(&flipped_nonce), Err(ProtoError::Aead));

        let mut truncated = sealed.clone();
        truncated.pop();
        assert_eq!(c.open(&truncated), Err(ProtoError::Aead));
    }

    #[test]
    fn open_rejects_the_wrong_key() {
        let sealed = crypto().seal(b"bolus", 0, 1, &NONCE).unwrap();
        assert_eq!(
            Crypto::new([0x43; KEY_LEN]).open(&sealed),
            Err(ProtoError::Aead)
        );
    }

    #[test]
    fn open_rejects_a_message_too_short_to_be_well_formed() {
        let c = crypto();
        for len in 0..MIN_SEALED_LEN {
            assert_eq!(
                c.open(&vec![0u8; len]),
                Err(ProtoError::SealedTooShort {
                    len,
                    min: MIN_SEALED_LEN
                })
            );
        }
    }

    #[test]
    fn different_nonces_give_different_ciphertexts() {
        let c = crypto();
        let mut other = NONCE;
        other[0] ^= 0xFF;
        let a = c.seal(b"same", 1, 1, &NONCE).unwrap();
        let b = c.seal(b"same", 1, 1, &other).unwrap();
        assert_ne!(a[..a.len() - NONCE_LEN], b[..b.len() - NONCE_LEN]);
        assert_eq!(c.open(&b), Ok(opened(b"same", 1, 1)));
    }

    #[test]
    fn debug_does_not_leak_the_key() {
        let crypto = crypto();
        assert_eq!(format!("{crypto:?}"), "Crypto { key: <redacted> }");
        assert_eq!(
            format!("{:?}", crypto.clone()),
            "Crypto { key: <redacted> }"
        );
    }

    #[test]
    fn a_full_size_message_survives_framing_and_crypto_together() {
        let c = crypto();
        let payload: Vec<u8> = (0..48u8).collect();
        let sealed = c.seal(&payload, 8, 1038, &NONCE).unwrap();
        let mut reassembler = Reassembler::default();
        let mut done = None;
        for frame in Frames::split(&sealed).unwrap() {
            done = reassembler.push(&frame).unwrap();
        }
        assert_eq!(c.open(&done.unwrap()), Ok(opened(&payload, 8, 1038)));
    }

    #[test]
    fn largest_payload_fits_the_frame_limit() {
        let sealed = crypto().seal(&[0; MAX_PAYLOAD_LEN], 0, 1, &NONCE).unwrap();
        assert_eq!(sealed.len(), MAX_MESSAGE_LEN);
        assert!(Frames::split(&sealed).is_ok());
    }

    #[test]
    fn refuses_a_payload_that_cannot_fit_the_frames() {
        assert_eq!(
            crypto().seal(&[0; MAX_PAYLOAD_LEN + 1], 0, 1, &NONCE),
            Err(ProtoError::PlaintextTooLong {
                len: MAX_PAYLOAD_LEN + 1,
                max: MAX_PAYLOAD_LEN
            })
        );
    }
}
