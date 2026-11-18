use alloc::vec;
use alloc::vec::Vec;

use pump_shared::{BleResult, Timestamp};

use super::codec::{crc16, le_i32, parse_safevar, status, with_crc};
use super::settings::ERR_VALUE_INVALID;
use super::YpsoSim;
use crate::protocol::{setting, ERR_VALUE_CORRUPT};

pub const BOLUS_AMOUNT_CAP: u16 = 112;
pub(super) const RUNNING: i32 = 10;
const STOPPED: i32 = 3;
const ERR_INTERNAL: u8 = 0x86;
const ERR_COMMAND_FAILED: u8 = 0x87;
const IMMEDIATE: u8 = 1;
const SLOW: u8 = 2;
const MIN_BOLUS: i32 = 10;
const SLOW_MINUTES: core::ops::RangeInclusive<i32> = 15..=720;
const MIN_TBR_MINUTES: i32 = 15;
const MINUTE: u64 = 60_000;
const CYCLE_MILLIS: i64 = 180_000;
const CYCLES_PER_HOUR_PERCENT: i32 = 2_000;

const BOLUS_DELAYED_RUNNING: u8 = 1;
const BOLUS_IMMEDIATE: u8 = 2;
const BOLUS_DELAYED: u8 = 3;
const TBR_RUNNING: u8 = 9;
const TBR: u8 = 10;
const PUMP_MODE_CHANGED: u8 = 14;
const BOLUS_COMBINED_RUNNING: u8 = 17;
const BOLUS_COMBINED: u8 = 18;
const BOLUS_IMMEDIATE_RUNNING: u8 = 19;
const BOLUS_IMMEDIATE_ABORT: u8 = 29;
const BOLUS_DELAYED_ABORT: u8 = 30;
const BOLUS_COMBINED_ABORT: u8 = 31;
const TBR_ABORT: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alarm {
    BatteryEmpty = 101,
    CartridgeEmpty = 104,
    Occlusion = 105,
}

#[derive(Debug)]
pub(super) struct Immediate {
    sequence: u32,
    total: i32,
    delivered: i32,
    start: u64,
    running: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Slow {
    sequence: u32,
    total: i32,
    immediate: i32,
    immediate_delivered: i32,
    delivered: i32,
    minutes: i32,
    start: u64,
}

impl Slow {
    fn kinds(&self) -> [u8; 3] {
        if self.immediate > 0 {
            [BOLUS_COMBINED_RUNNING, BOLUS_COMBINED, BOLUS_COMBINED_ABORT]
        } else {
            [BOLUS_DELAYED_RUNNING, BOLUS_DELAYED, BOLUS_DELAYED_ABORT]
        }
    }

    fn values(&self, amount: i32, immediate: i32, minutes: i32) -> [u16; 3] {
        let values = if self.immediate > 0 {
            [amount, immediate, minutes]
        } else {
            [amount, minutes, 0]
        };
        values.map(|value| value as u16)
    }

    fn extended(&self) -> i32 {
        self.total - self.immediate
    }

    fn next(&self) -> u64 {
        let duration = self.minutes as u64 * MINUTE;
        let (delivered, extended) = (self.delivered as u64, self.extended() as u64);
        self.start
            + if delivered < extended {
                ((delivered + 1) * duration).div_ceil(extended)
            } else {
                duration
            }
    }
}

#[derive(Debug)]
pub(super) struct Temp {
    sequence: u32,
    percent: i32,
    minutes: i32,
    start: u64,
}

impl Temp {
    fn end(&self) -> u64 {
        self.start + self.minutes as u64 * MINUTE
    }
}

#[derive(Debug, Clone, Copy)]
enum Tick {
    Bolus,
    SlowImmediate,
    Slow,
    TempEnd,
    Basal,
}

impl YpsoSim {
    pub fn stop(&mut self, now: Timestamp) {
        self.advance(now.as_millis());
        self.halt(now.as_millis());
    }

    pub fn run(&mut self, now: Timestamp) {
        self.advance(now.as_millis());
        if !self.running() {
            self.set_mode(RUNNING, now.as_millis());
        }
    }

    pub fn alarm(&mut self, alarm: Alarm, now: Timestamp) {
        self.advance(now.as_millis());
        self.raise(alarm, now.as_millis());
    }

    pub fn acknowledge_alarms(&mut self) {
        self.alarm_active = false;
    }

    fn running(&self) -> bool {
        self.setting(setting::PUMP_MODE_SELECTION) == RUNNING
    }

    fn set_mode(&mut self, mode: i32, at: u64) {
        self.settings.insert(setting::PUMP_MODE_SELECTION, mode);
        let stamp = self.stamp(at);
        self.events
            .push(stamp, PUMP_MODE_CHANGED, [mode as u16, 0, 0]);
    }

    fn raise(&mut self, alarm: Alarm, at: u64) {
        let stamp = self.stamp(at);
        self.alarms.push(stamp, alarm as u8, [0; 3]);
        self.alarm_active = true;
        self.halt(at);
    }

    fn halt(&mut self, at: u64) {
        self.end_bolus(BOLUS_IMMEDIATE_ABORT);
        self.abort_slow(at);
        self.end_temp(TBR_ABORT, at);
        if self.running() {
            self.set_mode(STOPPED, at);
        }
    }

    fn may_bolus(&self) -> bool {
        self.running() && self.reservoir > 0 && !self.alarm_active
    }

    fn amount_ok(&self, total: i32) -> bool {
        (MIN_BOLUS..=self.setting(BOLUS_AMOUNT_CAP)).contains(&total)
    }

    fn basal_rate(&self, hour: u16) -> i32 {
        let id = if self.setting(setting::BASAL_PROFILE_SELECTION) == 3 {
            setting::basal_a(hour)
        } else {
            setting::basal_b(hour)
        };
        self.setting(id).max(0)
    }

    fn hour(secs: i64) -> u16 {
        secs.div_euclid(3600).rem_euclid(24) as u16
    }

    pub(super) fn advance(&mut self, now: u64) {
        while let Some((at, tick)) = self.next_tick().filter(|(at, _)| *at <= now) {
            self.last = at;
            match tick {
                Tick::Bolus => self.bolus_tick(at),
                Tick::SlowImmediate => self.slow_immediate_tick(at),
                Tick::Slow => self.slow_tick(at),
                Tick::TempEnd => self.end_temp(TBR, at),
                Tick::Basal => self.basal_tick(at),
            }
        }
        self.last = self.last.max(now);
    }

    fn next_tick(&self) -> Option<(u64, Tick)> {
        let rate = self.bolus_millis_per_hundredth;
        let pump = self.clock * 1000 + self.last as i64;
        let boundary =
            ((pump.div_euclid(CYCLE_MILLIS) + 1) * CYCLE_MILLIS - self.clock * 1000) as u64;
        [
            self.running().then_some((boundary, Tick::Basal)),
            self.bolus
                .as_ref()
                .filter(|bolus| bolus.running)
                .map(|bolus| {
                    (
                        bolus.start + (bolus.delivered as u64 + 1) * rate,
                        Tick::Bolus,
                    )
                }),
            self.slow
                .filter(|slow| slow.immediate_delivered < slow.immediate)
                .map(|slow| {
                    let at = slow.start + (slow.immediate_delivered as u64 + 1) * rate;
                    (at, Tick::SlowImmediate)
                }),
            self.slow.map(|slow| (slow.next(), Tick::Slow)),
            self.temp.as_ref().map(|temp| (temp.end(), Tick::TempEnd)),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(at, _)| *at)
    }

    fn deliver(&mut self, hundredths: i32, at: u64) -> bool {
        if self.reservoir >= hundredths {
            self.reservoir -= hundredths;
            true
        } else {
            self.reservoir = self.reservoir.min(0);
            self.raise(Alarm::CartridgeEmpty, at);
            false
        }
    }

    fn bolus_tick(&mut self, at: u64) {
        if self.deliver(1, at) {
            let bolus = self.bolus.as_mut().expect("a bolus tick has a bolus");
            bolus.delivered += 1;
            if bolus.delivered == bolus.total {
                self.end_bolus(BOLUS_IMMEDIATE);
            }
        }
    }

    fn slow_immediate_tick(&mut self, at: u64) {
        if self.deliver(1, at) {
            self.slow
                .as_mut()
                .expect("a slow tick has a slow bolus")
                .immediate_delivered += 1;
        }
    }

    fn slow_tick(&mut self, at: u64) {
        let slow = self.slow.expect("a slow tick has a slow bolus");
        if slow.delivered == slow.extended() {
            self.slow = None;
            let values = slow.values(slow.total, slow.immediate, slow.minutes);
            self.events.rewrite(slow.sequence, slow.kinds()[1], values);
        } else if self.deliver(1, at) {
            self.slow = Some(Slow {
                delivered: slow.delivered + 1,
                ..slow
            });
        }
    }

    fn basal_tick(&mut self, at: u64) {
        let rate = self.basal_rate(Self::hour(self.pump_secs(at) - 1));
        let percent = self.temp.as_ref().map_or(100, |temp| temp.percent);
        self.basal_carry += rate * percent;
        let hundredths = self.basal_carry / CYCLES_PER_HOUR_PERCENT;
        self.basal_carry %= CYCLES_PER_HOUR_PERCENT;
        self.deliver(hundredths, at);
    }

    fn end_bolus(&mut self, kind: u8) {
        if let Some(bolus) = self.bolus.as_mut().filter(|bolus| bolus.running) {
            bolus.running = false;
            let (sequence, delivered) = (bolus.sequence, bolus.delivered);
            self.events
                .rewrite(sequence, kind, [delivered as u16, 0, 0]);
        }
    }

    fn abort_slow(&mut self, at: u64) {
        if let Some(slow) = self.slow.take() {
            let delivered = slow.immediate_delivered + slow.delivered;
            let elapsed = ((at - slow.start) / MINUTE) as i32;
            let values = slow.values(delivered, slow.immediate_delivered, elapsed);
            self.events.rewrite(slow.sequence, slow.kinds()[2], values);
        }
    }

    fn end_temp(&mut self, kind: u8, at: u64) {
        if let Some(temp) = self.temp.take() {
            let ran = (at - temp.start) / MINUTE;
            self.events
                .rewrite(temp.sequence, kind, [temp.percent as u16, ran as u16, 0]);
        }
    }

    pub(super) fn bolus_command(&mut self, data: &[u8], now: u64) -> BleResult {
        let command = if self.isc() {
            (data.len() == 15 && crc16(&data[..13]) == u16::from_le_bytes([data[13], data[14]]))
                .then(|| (le_i32(data, 0), le_i32(data, 4), le_i32(data, 8), data[12]))
        } else {
            parse_safevar(data).map(|total| (total, 0, 0, IMMEDIATE))
        };
        let Some((total, minutes, immediate, kind)) = command else {
            return BleResult::Status(ERR_VALUE_CORRUPT);
        };
        status(match (kind, total) {
            (IMMEDIATE, 0) => {
                self.end_bolus(BOLUS_IMMEDIATE);
                0
            }
            (SLOW, 0) => {
                self.abort_slow(now);
                0
            }
            (IMMEDIATE, _) => self.start_bolus(total, now),
            (SLOW, _) => self.start_slow(total, minutes, immediate, now),
            _ => ERR_VALUE_INVALID,
        })
    }

    fn start_bolus(&mut self, total: i32, now: u64) -> u8 {
        if !self.amount_ok(total) {
            return ERR_VALUE_INVALID;
        }
        if !self.may_bolus() || self.bolus.as_ref().is_some_and(|bolus| bolus.running) {
            return ERR_COMMAND_FAILED;
        }
        let stamp = self.stamp(now);
        let sequence = self
            .events
            .push(stamp, BOLUS_IMMEDIATE_RUNNING, [total as u16, 0, 0]);
        self.bolus = Some(Immediate {
            sequence,
            total,
            delivered: 0,
            start: now,
            running: true,
        });
        0
    }

    fn start_slow(&mut self, total: i32, minutes: i32, immediate: i32, now: u64) -> u8 {
        if !SLOW_MINUTES.contains(&minutes)
            || !self.amount_ok(total)
            || (immediate != 0 && !(MIN_BOLUS..=total - MIN_BOLUS).contains(&immediate))
        {
            return ERR_VALUE_INVALID;
        }
        if !self.may_bolus() || self.slow.is_some() {
            return ERR_COMMAND_FAILED;
        }
        let slow = Slow {
            sequence: 0,
            total,
            immediate,
            immediate_delivered: 0,
            delivered: 0,
            minutes,
            start: now,
        };
        let stamp = self.stamp(now);
        let values = slow.values(total, immediate, minutes);
        let sequence = self.events.push(stamp, slow.kinds()[0], values);
        self.slow = Some(Slow { sequence, ..slow });
        0
    }

    pub(super) fn tbr_command(&mut self, data: &[u8], now: u64) -> BleResult {
        let (Some(percent), Some(minutes)) = (
            data.get(..8).and_then(parse_safevar),
            data.get(8..).and_then(parse_safevar),
        ) else {
            return BleResult::Status(ERR_VALUE_CORRUPT);
        };
        if percent == 100 {
            self.end_temp(TBR, now);
            return BleResult::Done;
        }
        let long_scale = self.setting(setting::TBR_LONG_SCALE_MAX);
        let max_percent = long_scale.max(self.setting(setting::TBR_SHORT_SCALE_MAX));
        let max_minutes = self.setting(if percent <= long_scale {
            setting::TBR_LONG_DURATION_MAX
        } else {
            setting::TBR_SHORT_DURATION_MAX
        });
        status(
            if !(0..=max_percent).contains(&percent)
                || !(MIN_TBR_MINUTES..=max_minutes).contains(&minutes)
            {
                ERR_VALUE_INVALID
            } else if !self.running() {
                ERR_COMMAND_FAILED
            } else if self.temp.is_some() {
                ERR_INTERNAL
            } else {
                let stamp = self.stamp(now);
                let values = [percent as u16, minutes as u16, 0];
                let sequence = self.events.push(stamp, TBR_RUNNING, values);
                self.temp = Some(Temp {
                    sequence,
                    percent,
                    minutes,
                    start: now,
                });
                0
            },
        )
    }

    pub(super) fn system_status(&self, now: u64) -> Vec<u8> {
        let state = if self.running() { RUNNING } else { STOPPED };
        let mut bytes = vec![state as u8];
        bytes.extend_from_slice(&self.reservoir.to_le_bytes());
        bytes.push(self.battery);
        bytes.extend_from_slice(
            &self
                .basal_rate(Self::hour(self.pump_secs(now)))
                .to_le_bytes(),
        );
        let (percent, remaining) = self.temp.as_ref().map_or((100, 0), |temp| {
            (temp.percent, (temp.end() - now).div_ceil(MINUTE) as i32)
        });
        bytes.extend_from_slice(&percent.to_le_bytes());
        bytes.extend_from_slice(&remaining.to_le_bytes());
        if self.isc() {
            with_crc(bytes)
        } else {
            bytes
        }
    }

    pub(super) fn bolus_status(&self, now: u64) -> Vec<u8> {
        let (state, sequence, delivered, total) = self.bolus.as_ref().map_or((0, 0, 0, 0), |b| {
            (
                if b.running { 1 } else { 4 },
                b.sequence,
                b.delivered,
                b.total,
            )
        });
        let mut bytes = vec![state];
        if !self.isc() {
            for field in [delivered, total, sequence as i32] {
                bytes.extend_from_slice(&field.to_le_bytes());
            }
            return bytes;
        }
        for field in [sequence as i32, delivered, total] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        bytes.push(u8::from(self.slow.is_some()));
        let slow = self.slow.map_or([0; 7], |s| {
            [
                s.sequence as i32,
                s.immediate_delivered + s.delivered,
                s.total,
                s.immediate_delivered,
                s.immediate,
                ((now - s.start) / MINUTE) as i32,
                s.minutes,
            ]
        });
        for field in slow {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        with_crc(bytes)
    }
}
