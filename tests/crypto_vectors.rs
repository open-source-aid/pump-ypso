use pump_ypso::protocol::{KEY_LEN, NONCE_LEN};
use pump_ypso::{Crypto, Opened, ProtoError};

struct Case {
    key: u8,
    nonce: u8,
    payload_len: usize,
    reboot: u32,
    counter: u64,
    sealed: &'static str,
}

const CASES: &[Case] = &[
    Case {
        key: 0x42,
        nonce: 0x01,
        payload_len: 0,
        reboot: 0,
        counter: 0,
        sealed: "c1ee9232dca838384e1e9af51b1d40acfc98a7370135f6ddd55820701f202122232425262728292a2b2c2d2e2f30313233343536",
    },
    Case {
        key: 0x42,
        nonce: 0x02,
        payload_len: 1,
        reboot: 1,
        counter: 1,
        sealed: "5404decce15252e8d8187e9f1f950aa0b90ee0a569090c405fb2aaa3393e3f404142434445464748494a4b4c4d4e4f505152535455",
    },
    Case {
        key: 0x17,
        nonce: 0x03,
        payload_len: 16,
        reboot: 0x0102_0304,
        counter: 0x0A0B_0C0D_0E0F_1011,
        sealed: "9805628e653748a82a91418aab555aca61ab12704776be0a68b903ece0a9a33c8299e039a27ad64af2c1848e5d5e5f606162636465666768696a6b6c6d6e6f7071727374",
    },
    Case {
        key: 0x17,
        nonce: 0x04,
        payload_len: 52,
        reboot: 7,
        counter: 1000,
        sealed: "74c47ff14c6f4b0aaf835e0526af1a99e516d63afdb4e2692947585e3d7a63c080baeb6596b75ce575a504791be07d12f25cd3f457a23d4e06fe06bdcc8005d95c70badc1fd6cf1d377d87843f47ed1f7c7d7e7f808182838485868788898a8b8c8d8e8f90919293",
    },
    Case {
        key: 0xA5,
        nonce: 0x05,
        payload_len: 130,
        reboot: u32::MAX,
        counter: u64::MAX,
        sealed: "c89d8353d7ce1ffc816efb38d0426fa561fe4183fe6fcdf47daf937894a3b09b334522029ecfef3ab68606a7c2520d82960c3dd45ee6be8ef78587e7af1270539ae48e5f9445717b143d4c1b1158d33a5d676f3172365f4e6d9d649096ba35a9240e1b3c8dd63a304a4eb9c0cd86f0cff6660a1cd9b96af3231e5b8dae54eaa980558319dff98d43ec88289c0adb2544858193a84402f7223f4b44f49b5b9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2",
    },
];

fn key(seed: u8) -> [u8; KEY_LEN] {
    core::array::from_fn(|i| seed.wrapping_add(i as u8))
}

fn nonce(seed: u8) -> [u8; NONCE_LEN] {
    core::array::from_fn(|i| seed.wrapping_mul(31).wrapping_add(i as u8))
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 13 + 5) as u8).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn every_vector_seals_to_the_pinned_bytes() {
    for case in CASES {
        let sealed = Crypto::new(key(case.key))
            .seal(
                &payload(case.payload_len),
                case.reboot,
                case.counter,
                &nonce(case.nonce),
            )
            .unwrap();
        assert_eq!(
            hex(&sealed),
            case.sealed,
            "payload_len {}",
            case.payload_len
        );
    }
}

#[test]
fn every_pinned_vector_opens_to_its_payload_and_counters() {
    for case in CASES {
        let crypto = Crypto::new(key(case.key));
        let sealed = unhex(case.sealed);
        assert_eq!(
            crypto.open(&sealed),
            Ok(Opened {
                payload: payload(case.payload_len),
                reboot: case.reboot,
                counter: case.counter,
            })
        );
        for i in [0, sealed.len() - NONCE_LEN - 1, sealed.len() - 1] {
            let mut tampered = sealed.clone();
            tampered[i] ^= 0x01;
            assert_eq!(crypto.open(&tampered), Err(ProtoError::Aead), "byte {i}");
        }
    }
}
