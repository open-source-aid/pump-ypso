use alloc::vec::Vec;

use super::crc::{crc32, crc32_update};
use crate::protocol::{Crypto, KEY_LEN, NONCE_LEN};
use crate::Counters;

const HEADER_LEN: usize = 92;
const CRC_LEN: usize = 4;
const PASSWORD_LEN: usize = 16;
const MAX_CREDENTIALS: usize = 8;
const MAX_CODE_LEN: usize = 8;
const PREFIX_LEN: usize = 16;
const COUNTERS_LEN: usize = 24;
const NONCE_STATE_LEN: usize = PREFIX_LEN + 8;
const NONCE_BLOCK: u64 = 64;

#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) mac: [u8; 6],
    pub(crate) serial: u32,
    pub(crate) crypto: Option<Crypto>,
    pub(crate) fingerprint: u32,
    pub(crate) counters: (i64, i64, u32),
    pub(crate) prefix: [u8; PREFIX_LEN],
    pub(crate) passwords: Vec<(u8, [u8; PASSWORD_LEN])>,
}

impl Session {
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_LEN + CRC_LEN || bytes[..2] != *b"YB" {
            return None;
        }
        let (body, crc) = bytes.split_at(bytes.len() - CRC_LEN);
        if crc32(body).to_le_bytes() != crc {
            return None;
        }
        let keyless = match (body[2], body[3]) {
            (1 | 3, 0) => false,
            (4, 1) if cfg!(feature = "opn") => true,
            _ => return None,
        };
        let validity = match (body[2], body[74]) {
            (_, 0..=2) | (3, 3) => le_i64(&body[66..74]),
            _ => return None,
        };
        if keyless && (validity != 0 || body[74] != 0) {
            return None;
        }
        let count = usize::from(body[91]);
        let mut passwords = Vec::new();
        let mut at = HEADER_LEN;
        for _ in 0..count.min(MAX_CREDENTIALS + 1) {
            let len = usize::from(body.get(at).copied().unwrap_or(0));
            let end = at + 1 + len + PASSWORD_LEN;
            if !(1..=MAX_CODE_LEN).contains(&len)
                || end > body.len()
                || !body[at + 1..at + 1 + len]
                    .iter()
                    .all(|b| (0x21..0x7F).contains(b))
            {
                return None;
            }
            passwords.push((body[at + 1], array(&body[end - PASSWORD_LEN..])));
            at = end;
        }
        let (write, read) = (le_i64(&body[46..54]), le_i64(&body[54..62]));
        let prefix: [u8; PREFIX_LEN] = array(&body[75..]);
        if count > MAX_CREDENTIALS
            || at != body.len()
            || write < -1
            || read < -1
            || prefix == [0; PREFIX_LEN]
        {
            return None;
        }
        let key: [u8; KEY_LEN] = array(&body[14..]);
        Some(Self {
            mac: array(&body[4..]),
            serial: le_u32(&body[10..14]),
            crypto: (!keyless).then(|| Crypto::new(key)),
            fingerprint: (!crc32_update(crc32_update(!0, b"ypso counters key"), &key)).max(1),
            counters: (write, read, le_u32(&body[62..66])),
            prefix,
            passwords,
        })
    }

    /// Serialise the session into the `"YB"` blob [`Session::decode`] reads back, with `key` as the
    /// session key. The offline pair uses this to persist a freshly agreed session; `validity` is
    /// not kept (the engine does not use it).
    pub(crate) fn encode(&self, key: &[u8; KEY_LEN]) -> Vec<u8> {
        let mut body = alloc::vec![b'Y', b'B', 1, 0];
        body.extend_from_slice(&self.mac);
        body.extend_from_slice(&self.serial.to_le_bytes());
        body.extend_from_slice(key);
        body.extend_from_slice(&self.counters.0.to_le_bytes());
        body.extend_from_slice(&self.counters.1.to_le_bytes());
        body.extend_from_slice(&self.counters.2.to_le_bytes());
        body.extend_from_slice(&0i64.to_le_bytes()); // validity (unused by the engine)
        body.push(0); // validity kind
        body.extend_from_slice(&self.prefix);
        body.push(self.passwords.len() as u8);
        for (code, password) in &self.passwords {
            body.push(1); // code length (one byte, as decode keeps only the first)
            body.push(*code);
            body.extend_from_slice(password);
        }
        let crc = crc32(&body);
        body.extend_from_slice(&crc.to_le_bytes());
        body
    }

    pub(crate) fn resume(&mut self, stored: Option<&[u8]>) {
        let Some(bytes) = stored.filter(|b| b.len() == COUNTERS_LEN) else {
            return;
        };
        let saved = (
            le_i64(&bytes[..8]),
            le_i64(&bytes[8..16]),
            le_u32(&bytes[16..20]),
        );
        if le_u32(&bytes[20..24]) != self.fingerprint || saved.0 < -1 || saved.1 < -1 {
            return;
        }
        let blob = self.counters;
        self.counters = match saved.2.cmp(&blob.2) {
            core::cmp::Ordering::Greater => saved,
            core::cmp::Ordering::Equal => (blob.0.max(saved.0), blob.1.max(saved.1), blob.2),
            core::cmp::Ordering::Less => blob,
        };
    }

    pub(crate) fn counters_record(&self, counters: &Counters) -> Vec<u8> {
        let (write, read) = counters.as_session_pair();
        [
            &write.to_le_bytes()[..],
            &read.to_le_bytes(),
            &counters.epoch().to_le_bytes(),
            &self.fingerprint.to_le_bytes(),
        ]
        .concat()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Nonces {
    prefix: [u8; PREFIX_LEN],
    next: u64,
    reserved: u64,
}

impl Nonces {
    pub(crate) fn resume(prefix: [u8; PREFIX_LEN], stored: Option<&[u8]>) -> Self {
        let start = match stored {
            Some(state) if state.len() == NONCE_STATE_LEN && state[..PREFIX_LEN] == prefix => {
                u64::from_le_bytes(array(&state[PREFIX_LEN..]))
            }
            _ => 0,
        };
        Self {
            prefix,
            next: start,
            reserved: start,
        }
    }

    pub(crate) fn reservation(&self) -> Option<Vec<u8>> {
        (self.next >= self.reserved).then(|| {
            let bound = self.next.saturating_add(NONCE_BLOCK);
            [&self.prefix[..], &bound.to_le_bytes()].concat()
        })
    }

    pub(crate) fn reserved(&mut self) {
        self.reserved = self.next.saturating_add(NONCE_BLOCK);
    }

    pub(crate) fn take(&mut self) -> [u8; NONCE_LEN] {
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..PREFIX_LEN].copy_from_slice(&self.prefix);
        nonce[PREFIX_LEN..].copy_from_slice(&self.next.to_le_bytes());
        self.next += 1;
        nonce
    }
}

fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes[..N]);
    out
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(array(bytes))
}

fn le_i64(bytes: &[u8]) -> i64 {
    i64::from_le_bytes(array(bytes))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;

    pub(crate) fn blob(edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut body = b"YB".to_vec();
        body.extend([1, 0, 0xEC, 0x2A, 0xF0, 1, 2, 3]);
        body.extend(10_155_555u32.to_le_bytes());
        body.extend([0x42; 32]);
        body.extend((-1i64).to_le_bytes());
        body.extend(5i64.to_le_bytes());
        body.extend(2u32.to_le_bytes());
        body.extend(0i64.to_le_bytes());
        body.push(0);
        body.extend([7; 16]);
        body.extend([1, 2, b'O', b'2']);
        body.extend([0x5A; 16]);
        edit(&mut body);
        let crc = crc32(&body);
        body.extend(crc.to_le_bytes());
        body
    }

    #[test]
    fn decodes_a_session_blob() {
        let session = Session::decode(&blob(|_| {})).unwrap();
        assert_eq!(session.serial, 10_155_555);
        assert_eq!(session.mac, [0xEC, 0x2A, 0xF0, 1, 2, 3]);
        assert_eq!(session.counters, (-1, 5, 2));
        assert_eq!(session.passwords, vec![(b'O', [0x5A; 16])]);
        assert!(session.crypto.is_some());
        assert!(Session::decode(&blob(|b| b[2] = 3)).is_some());
        assert!(Session::decode(&blob(|b| {
            b[2] = 3;
            b[74] = 3
        }))
        .is_some());
    }

    #[test]
    fn refuses_every_malformed_blob() {
        let mut short = blob(|_| {});
        short.truncate(50);
        let mut bad_crc = blob(|_| {});
        bad_crc[95] ^= 1;
        let refused = [
            short,
            blob(|b| b[0] = b'X'),
            bad_crc,
            blob(|b| b[2] = 2),
            blob(|b| b[3] = 1),
            blob(|b| b[74] = 3),
            blob(|b| b[74] = 9),
            blob(|b| b[91] = 9),
            blob(|b| b[92] = 0),
            blob(|b| b[92] = 9),
            blob(|b| b[93] = b' '),
            blob(|b| b[91] = 2),
            blob(|b| b.push(0)),
            blob(|b| b[46..54].copy_from_slice(&(-2i64).to_le_bytes())),
            blob(|b| b[54..62].copy_from_slice(&(-2i64).to_le_bytes())),
            blob(|b| b[75..91].copy_from_slice(&[0; 16])),
            blob(|b| {
                b[2] = 4;
                b[3] = 1;
                b[66] = 1
            }),
            blob(|b| {
                b[2] = 4;
                b[3] = 1;
                b[74] = 1
            }),
        ];
        for bytes in refused {
            assert!(Session::decode(&bytes).is_none(), "{bytes:?}");
        }
        assert_eq!(
            Session::decode(&blob(|b| {
                b[2] = 4;
                b[3] = 1
            }))
            .map(|s| s.crypto.is_none()),
            cfg!(feature = "opn").then_some(true)
        );
    }

    #[test]
    fn merges_stored_counters_forward_for_the_same_key() {
        let base = Session::decode(&blob(|_| {})).unwrap();
        let record = |write: i64, read: i64, epoch: u32, key: u32| {
            [
                &write.to_le_bytes()[..],
                &read.to_le_bytes(),
                &epoch.to_le_bytes(),
                &key.to_le_bytes(),
            ]
            .concat()
        };
        let key = base.fingerprint;
        let resumed = |stored: Option<Vec<u8>>| {
            let mut session = base.clone();
            session.resume(stored.as_deref());
            session.counters
        };
        assert_eq!(resumed(None), (-1, 5, 2));
        assert_eq!(resumed(Some(vec![1, 2])), (-1, 5, 2));
        assert_eq!(resumed(Some(record(9, 9, 2, key + 1))), (-1, 5, 2));
        assert_eq!(resumed(Some(record(-2, 9, 2, key))), (-1, 5, 2));
        assert_eq!(resumed(Some(record(9, -2, 2, key))), (-1, 5, 2));
        assert_eq!(resumed(Some(record(9, 3, 2, key))), (9, 5, 2));
        assert_eq!(resumed(Some(record(1, 1, 3, key))), (1, 1, 3));
        assert_eq!(resumed(Some(record(90, 90, 1, key))), (-1, 5, 2));
    }

    #[test]
    fn nonces_resume_above_the_stored_bound() {
        let prefix = [7; 16];
        let mut fresh = Nonces::resume(prefix, None);
        assert_eq!(fresh.reservation().unwrap()[16..], 64u64.to_le_bytes());
        fresh.reserved();
        assert_eq!(fresh.reservation(), None);
        assert_eq!(fresh.take()[16..], 0u64.to_le_bytes());
        let stored = [&prefix[..], &64u64.to_le_bytes()].concat();
        let mut resumed = Nonces::resume(prefix, Some(&stored));
        resumed.reserved();
        assert_eq!(resumed.take()[16..], 64u64.to_le_bytes());
        assert_eq!(
            Nonces::resume([8; 16], Some(&stored)),
            Nonces::resume([8; 16], None)
        );
        assert_eq!(
            Nonces::resume(prefix, Some(&stored[1..])),
            Nonces::resume(prefix, None)
        );
    }
}
