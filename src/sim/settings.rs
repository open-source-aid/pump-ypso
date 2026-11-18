use alloc::vec;
use alloc::vec::Vec;

use pump_shared::BleResult;

use super::codec::{civil_date, days_since_2000, parse_safevar, safevar, SECONDS_PER_DAY};
use super::YpsoSim;
use crate::protocol::{setting, ERR_INVALID_ID, ERR_VALUE_CORRUPT};

pub(super) const ERR_VALUE_INVALID: u8 = 0x82;
const BASAL_PROFILE_A_CHANGED: u8 = 7;
const BASAL_PROFILE_B_CHANGED: u8 = 8;
const DATE_CHANGED: u8 = 12;
const TIME_CHANGED: u8 = 13;

const SETTING_IDS: core::ops::RangeInclusive<i32> = 1..=122;

impl YpsoSim {
    pub(super) fn pump_secs(&self, at: u64) -> i64 {
        (self.clock * 1000 + at as i64).div_euclid(1000)
    }

    pub(super) fn stamp(&self, at: u64) -> u32 {
        self.pump_secs(at) as u32
    }

    pub(super) fn select_setting(&mut self, data: &[u8]) -> BleResult {
        match parse_safevar(data) {
            Some(id) => {
                self.selected = id;
                BleResult::Done
            }
            None => {
                self.selected = 0;
                BleResult::Status(ERR_VALUE_CORRUPT)
            }
        }
    }

    fn selection(&self) -> Result<u16, u8> {
        if !SETTING_IDS.contains(&self.selected) {
            return Err(ERR_INVALID_ID);
        }
        self.access().map_or(Ok(self.selected as u16), Err)
    }

    pub(super) fn setting_value(&mut self, _now: u64) -> Result<Vec<u8>, BleResult> {
        self.selection()
            .map(|id| safevar(self.setting(id)).to_vec())
            .map_err(BleResult::Status)
    }

    pub(super) fn write_setting(&mut self, data: &[u8], now: u64) -> BleResult {
        let id = match self.selection() {
            Ok(id) => id,
            Err(code) => return BleResult::Status(code),
        };
        let Some(value) = parse_safevar(data) else {
            return BleResult::Status(ERR_VALUE_CORRUPT);
        };
        let old = self.settings.insert(id, value).unwrap_or_default();
        let slot = id.wrapping_sub(setting::BASAL_PROFILE_A_01);
        if slot < 2 * setting::BASAL_SLOTS {
            let kind = [BASAL_PROFILE_A_CHANGED, BASAL_PROFILE_B_CHANGED]
                [usize::from(slot / setting::BASAL_SLOTS)];
            let values = [slot % setting::BASAL_SLOTS, value as u16, old as u16];
            let stamp = self.stamp(now);
            self.events.push(stamp, kind, values);
        }
        BleResult::Done
    }

    pub(super) fn select_counter(&mut self, data: &[u8]) -> BleResult {
        let Some(id) = parse_safevar(data) else {
            return BleResult::Status(ERR_VALUE_CORRUPT);
        };
        match u16::try_from(id)
            .ok()
            .filter(|id| self.counters.contains_key(id))
        {
            Some(id) => {
                self.counter = Some(id);
                BleResult::Done
            }
            None => BleResult::Status(ERR_INVALID_ID),
        }
    }

    pub(super) fn counter_value(&mut self, _now: u64) -> Result<Vec<u8>, BleResult> {
        self.counter
            .and_then(|id| self.counters.get(&id))
            .map(|value| safevar(*value).to_vec())
            .ok_or(BleResult::Failed)
    }

    pub(super) fn date(&mut self, now: u64) -> Result<Vec<u8>, BleResult> {
        let (year, month, day) = civil_date(self.pump_secs(now).div_euclid(SECONDS_PER_DAY));
        let [lo, hi] = (year as u16).to_le_bytes();
        Ok(vec![lo, hi, month as u8, day as u8])
    }

    pub(super) fn time(&mut self, now: u64) -> Result<Vec<u8>, BleResult> {
        let of_day = self.pump_secs(now).rem_euclid(SECONDS_PER_DAY);
        Ok([of_day / 3600, of_day / 60 % 60, of_day % 60]
            .map(|part| part as u8)
            .to_vec())
    }

    pub(super) fn write_time(&mut self, data: &[u8], now: u64) -> BleResult {
        let [hour, minute, second] = *data else {
            return BleResult::Status(ERR_VALUE_INVALID);
        };
        if hour > 23 || minute > 59 || second > 59 {
            return BleResult::Status(ERR_VALUE_INVALID);
        }
        let secs = self.pump_secs(now);
        let set = i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
        self.set_clock(
            secs - secs.rem_euclid(SECONDS_PER_DAY) + set,
            now,
            TIME_CHANGED,
            [hour, minute, second],
        )
    }

    pub(super) fn write_date(&mut self, data: &[u8], now: u64) -> BleResult {
        let [lo, hi, month, day] = *data else {
            return BleResult::Status(ERR_VALUE_INVALID);
        };
        let year = i64::from(u16::from_le_bytes([lo, hi]));
        let days = days_since_2000(year, i64::from(month), i64::from(day));
        if days < 0 || civil_date(days) != (year, i64::from(month), i64::from(day)) {
            return BleResult::Status(ERR_VALUE_INVALID);
        }
        let secs = self.pump_secs(now);
        let values = [day, month, (year - 2000) as u8];
        self.set_clock(
            days * SECONDS_PER_DAY + secs.rem_euclid(SECONDS_PER_DAY),
            now,
            DATE_CHANGED,
            values,
        )
    }

    fn set_clock(&mut self, secs: i64, now: u64, kind: u8, values: [u8; 3]) -> BleResult {
        let stamp = self.stamp(now);
        self.events.push(stamp, kind, values.map(u16::from));
        self.clock += secs - self.pump_secs(now);
        BleResult::Done
    }
}
