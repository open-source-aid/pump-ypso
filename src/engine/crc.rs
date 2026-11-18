const POLY: u32 = 0xEDB8_8320;

pub(crate) fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (POLY & (crc & 1).wrapping_neg());
        }
    }
    crc
}

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    !crc32_update(!0, bytes)
}

pub fn crc16(value: &[u8]) -> u16 {
    let mut buf = value.to_vec();
    buf.resize(value.len().div_ceil(4) * 4, 0);
    for block in buf.chunks_exact_mut(4) {
        block.reverse();
        block
            .iter_mut()
            .for_each(|byte| *byte = byte.reverse_bits());
    }
    (crc32_update(!0, &buf).reverse_bits() & 0xFFFF) as u16
}

pub(crate) fn with_crc16(mut value: alloc::vec::Vec<u8>) -> alloc::vec::Vec<u8> {
    let crc = crc16(&value);
    value.extend_from_slice(&crc.to_le_bytes());
    value
}

pub(crate) fn crc16_ok(value: &[u8]) -> bool {
    value.len() >= 2 && {
        let (body, crc) = value.split_at(value.len() - 2);
        crc16(body).to_le_bytes() == crc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn crc16_matches_the_reference_vector() {
        let data: Vec<u8> = (0..=0x10).collect();
        assert_eq!(crc16(&data), 0x8F1E);
        assert_eq!(with_crc16(data.clone())[17..], [0x1E, 0x8F]);
        assert!(crc16_ok(&with_crc16(data)));
        assert!(!crc16_ok(&[1]));
        assert!(!crc16_ok(&[1, 2, 3]));
    }

    #[test]
    fn crc32_is_the_zlib_one() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
