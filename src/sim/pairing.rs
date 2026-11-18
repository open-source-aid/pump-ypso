use chacha20::cipher::consts::U10;
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use pump_shared::BleResult;

use super::YpsoSim;
use crate::protocol::{ERR_COUNTER_ERROR, ERR_ENCRYPTION_FAILED, ERR_VALUE_CORRUPT, KEY_LEN};

const BLOB_LEN: usize = 116;
const PLAINTEXT_LEN: usize = 76;
const COOLDOWN_MILLIS: u64 = 300_000;

pub(super) fn public(secret: [u8; KEY_LEN]) -> [u8; KEY_LEN] {
    MontgomeryPoint::mul_base_clamped(secret).to_bytes()
}

pub(super) fn shared(secret: [u8; KEY_LEN], peer: [u8; KEY_LEN]) -> [u8; KEY_LEN] {
    MontgomeryPoint(peer).mul_clamped(secret).to_bytes()
}

fn derive(secret: [u8; KEY_LEN], peer: [u8; KEY_LEN]) -> [u8; KEY_LEN] {
    chacha20::hchacha::<U10>(&shared(secret, peer).into(), &[0u8; 16].into()).into()
}

impl YpsoSim {
    pub fn pump_public(&self) -> [u8; KEY_LEN] {
        public(self.pump_secret)
    }

    fn pairing_gate(&self, now: u64) -> Option<u8> {
        self.security()
            .or_else(|| self.access())
            .or_else(|| self.key.is_none().then_some(ERR_VALUE_CORRUPT))
            .or_else(|| self.pause(now))
    }

    pub(super) fn offer(&mut self, now: u64) -> BleResult {
        if let Some(code) = self.pairing_gate(now) {
            return BleResult::Status(code);
        }
        let mut offer = self.challenge.to_vec();
        offer.extend_from_slice(&self.pump_public());
        self.framed(&offer)
    }

    pub(super) fn exchange(&mut self, blob: &[u8], now: u64) -> u8 {
        if let Some(code) = self.pairing_gate(now) {
            return code;
        }
        if blob.len() != BLOB_LEN {
            return ERR_ENCRYPTION_FAILED;
        }
        let (sealed, nonce) = blob.split_at(PLAINTEXT_LEN + 16);
        let (ciphertext, tag) = sealed.split_at(PLAINTEXT_LEN);
        let blob_key = derive(self.pump_secret, public(self.server_secret));
        let mut plaintext = ciphertext.to_vec();
        let opened = XChaCha20Poly1305::new(&blob_key.into()).decrypt_in_place_detached(
            XNonce::from_slice(nonce),
            &[],
            &mut plaintext,
            Tag::from_slice(tag),
        );
        if opened.is_err() || plaintext[..KEY_LEN] != self.challenge {
            return ERR_ENCRYPTION_FAILED;
        }
        let mut counter = [0u8; 8];
        counter.copy_from_slice(&plaintext[68..]);
        let counter = u64::from_le_bytes(counter);
        if counter <= self.pairing_counter {
            return ERR_COUNTER_ERROR;
        }
        let mut peer = [0u8; KEY_LEN];
        peer.copy_from_slice(&plaintext[36..68]);
        self.key = Some(derive(self.pump_secret, peer));
        self.reboot += 1;
        self.pump_counter = 0;
        self.app_counter = 0;
        self.pairing_counter = counter;
        self.cooldown_until = now + COOLDOWN_MILLIS;
        0
    }
}
