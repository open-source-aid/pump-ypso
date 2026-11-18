use alloc::vec::Vec;

use pump_shared::BleResult;

pub(super) const SECONDS_PER_DAY: i64 = 86_400;
const DAYS_1970_TO_2000: i64 = 10_957;

pub(super) fn status(code: u8) -> BleResult {
    match code {
        0 => BleResult::Done,
        code => BleResult::Status(code),
    }
}

pub(super) fn version(text: &str) -> f64 {
    text.trim().parse().unwrap_or(0.0)
}

pub(super) fn safevar(value: i32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&value.to_le_bytes());
    bytes[4..].copy_from_slice(&(!value).to_le_bytes());
    bytes
}

pub(super) fn parse_safevar(data: &[u8]) -> Option<i32> {
    let bytes: [u8; 8] = data.try_into().ok()?;
    let value = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (bytes == safevar(value)).then_some(value)
}

pub(super) fn le_i32(data: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut padded = data.to_vec();
    padded.resize(data.len().div_ceil(4) * 4, 0);
    let mut crc = u32::MAX;
    for block in padded.chunks(4) {
        for byte in block.iter().rev() {
            crc ^= u32::from(byte.reverse_bits());
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            }
        }
    }
    crc.reverse_bits() as u16
}

pub(super) fn with_crc(mut bytes: Vec<u8>) -> Vec<u8> {
    let crc = crc16(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes
}

pub(super) fn civil_date(days_since_2000: i64) -> (i64, i64, i64) {
    let shifted = days_since_2000 + DAYS_1970_TO_2000 + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = (shifted_month + 2) % 12 + 1;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub(super) fn days_since_2000(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468 - DAYS_1970_TO_2000
}
