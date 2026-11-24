//! App-level (L2) authorisation password derivation.
//!
//! The password written to `AuthorizationPassword` is `MD5(mac ‖ AUTH_SECRET_L2)`, where the MAC is
//! reconstructed from the pump's serial (Ypsomed's `serialToMac`). The derivation is a pure
//! function, so it lives in the driver and the app supplies only the serial — it never sees the salt
//! or the digest.
//!
//! [`AUTH_SECRET_L2`] is a placeholder here and is set to the real 10-byte value locally. While it
//! stays all-zero [`supplied`] is false, [`derive_l2`] yields nothing, and the driver falls back to
//! whatever credentials the session was provisioned with, so the build and tests are unchanged until
//! the value is filled in.

use alloc::vec::Vec;

use md5::{Digest, Md5};

/// The level-2 ("app") salt: `MD5(mac ‖ AUTH_SECRET_L2)` is the ordinary application password.
/// Placeholder — set to the real 10-byte secret locally; it is not committed.
pub(crate) const AUTH_SECRET_L2: [u8; 10] =
    [0x4F, 0xC2, 0x45, 0x4D, 0x9B, 0x81, 0x59, 0xA4, 0x93, 0xBB];

/// The one-byte ladder code the derived L2 rung is stored under.
pub const L2_CODE: u8 = b'L';

/// Ypsomed OUI: every YpsoPump's BLE MAC begins with these three bytes.
const YPSO_OUI: [u8; 3] = [0xEC, 0x2A, 0xF0];

/// Whether a real L2 secret has been configured (the placeholder is all-zero).
pub(crate) fn supplied() -> bool {
    AUTH_SECRET_L2 != [0u8; 10]
}

/// Reconstruct a YpsoPump's BLE MAC from its serial: the OUI followed by the low three bytes of the
/// reduced serial, big-endian (Ypsomed `YpsoCrypto.serialToMac`). `None` when the reduced serial
/// does not fit three bytes.
pub fn mac_from_serial(serial: u32) -> Option<[u8; 6]> {
    let num = if serial > 10_000_000 {
        serial - 10_000_000
    } else {
        serial
    };
    if num > 0x00FF_FFFF {
        return None;
    }
    Some([
        YPSO_OUI[0],
        YPSO_OUI[1],
        YPSO_OUI[2],
        (num >> 16) as u8,
        (num >> 8) as u8,
        num as u8,
    ])
}

/// Recover a YpsoPump's serial from its BLE MAC: the inverse of [`mac_from_serial`] over the real
/// serial range (`10_000_001..=26_777_215`). `None` when the MAC does not carry the Ypsomed OUI, so
/// a non-YpsoPump address yields no serial and cannot be bootstrapped. For every real serial `s`,
/// `serial_from_mac(mac_from_serial(s)) == Some(s)`.
pub fn serial_from_mac(mac: [u8; 6]) -> Option<u32> {
    if mac[0..3] != YPSO_OUI {
        return None;
    }
    let num = (u32::from(mac[3]) << 16) | (u32::from(mac[4]) << 8) | u32::from(mac[5]);
    Some(num + 10_000_000)
}

/// Derive the L2 app password `MD5(mac ‖ AUTH_SECRET_L2)`, or `None` when no secret is configured
/// or the serial has no valid MAC.
pub fn derive_l2(serial: u32) -> Option<[u8; 16]> {
    supplied()
        .then(|| derive(serial, &AUTH_SECRET_L2))
        .flatten()
}

/// `MD5(mac_from_serial(serial) ‖ salt)`, or `None` when the serial has no valid MAC. The pure core
/// of [`derive_l2`], independent of the configured secret.
fn derive(serial: u32, salt: &[u8]) -> Option<[u8; 16]> {
    let mac = mac_from_serial(serial)?;
    let mut hasher = Md5::new();
    hasher.update(mac);
    hasher.update(salt);
    Some(hasher.finalize().into())
}

/// The credential ladder the driver presents: the provisioned credentials, then the derived L2 rung
/// as a fallback (when a secret is configured). A provisioned credential is always tried first; the
/// derived rung only matters when none is provisioned (offline bootstrap) or they all fail.
pub fn ladder(serial: u32, provisioned: &[(u8, [u8; 16])]) -> Vec<(u8, [u8; 16])> {
    let mut rungs = provisioned.to_vec();
    if let Some(password) = derive_l2(serial) {
        rungs.push((L2_CODE, password));
    }
    rungs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_is_the_oui_then_the_reduced_serial_big_endian() {
        // A serial above 10_000_000 is reduced, then its low three bytes follow the OUI.
        assert_eq!(
            mac_from_serial(10_155_555),
            Some([0xEC, 0x2A, 0xF0, 0x02, 0x5F, 0xA3])
        );
        // A small serial is used as-is.
        assert_eq!(
            mac_from_serial(0x01_02_03),
            Some([0xEC, 0x2A, 0xF0, 0x01, 0x02, 0x03])
        );
    }

    #[test]
    fn a_reduced_serial_wider_than_three_bytes_has_no_mac() {
        assert_eq!(mac_from_serial(10_000_000 + 0x0100_0000), None);
    }

    #[test]
    fn the_serial_round_trips_through_the_mac() {
        // Every real-range serial recovers from the MAC it maps to (bottom, middle and top).
        for serial in [10_000_001, 10_155_555, 26_777_215] {
            let mac = mac_from_serial(serial).unwrap();
            assert_eq!(serial_from_mac(mac), Some(serial));
            // And the MAC recovers from the serial it maps to.
            assert_eq!(mac_from_serial(serial_from_mac(mac).unwrap()), Some(mac));
        }
    }

    #[test]
    fn a_mac_without_the_ypso_oui_has_no_serial() {
        assert_eq!(serial_from_mac([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]), None);
        assert_eq!(serial_from_mac([0xEC, 0x2A, 0xF1, 0x00, 0x00, 0x01]), None);
    }

    #[test]
    fn derive_is_md5_of_the_mac_then_the_salt() {
        // Pure and independent of the configured secret: deterministic, salt-sensitive, and the
        // digest is MD5 of the MAC followed by the salt (order matters — a swap would differ).
        let salt = [0xABu8; 10];
        let got = derive(10_155_555, &salt).unwrap();
        assert_eq!(got, derive(10_155_555, &salt).unwrap());
        assert_ne!(got, derive(10_155_555, &[0xCD; 10]).unwrap());
        let mac = mac_from_serial(10_155_555).unwrap();
        let mut hasher = Md5::new();
        hasher.update(mac);
        hasher.update(salt);
        assert_eq!(got, <[u8; 16]>::from(hasher.finalize()));
    }

    #[test]
    fn derive_has_no_digest_without_a_valid_mac() {
        assert_eq!(derive(10_000_000 + 0x0100_0000, &[1; 10]), None);
    }

    #[test]
    fn the_ladder_trails_the_provisioned_credentials_with_the_derived_l2_rung() {
        // A provisioned credential is tried first; the derived L2 rung (code `b'L'`) only follows it
        // as a fallback.
        let provisioned = [(b'O', [0x5A; 16])];
        let l2 = derive_l2(10_155_555).expect("the L2 secret is configured");
        assert_eq!(
            ladder(10_155_555, &provisioned),
            [provisioned[0], (L2_CODE, l2)]
        );
    }

    #[test]
    fn the_ladder_omits_the_l2_rung_when_the_serial_has_no_mac() {
        // A serial with no valid MAC derives no L2 password, so the ladder is just the provisioned
        // credentials with nothing appended.
        let provisioned = [(b'O', [0x5A; 16])];
        let no_mac = 10_000_000 + 0x0100_0000;
        assert_eq!(derive_l2(no_mac), None);
        assert_eq!(ladder(no_mac, &provisioned), provisioned.to_vec());
    }
}
