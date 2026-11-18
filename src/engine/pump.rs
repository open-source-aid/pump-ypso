use alloc::vec;
use core::time::Duration;

use pump_shared::{
    Basal, BasalRate, Bolus, Bounds, Capabilities, Extended, Failure, InsulinAmount, Limits,
    Percent, PumpMode, PumpStatus, PumpTime, TempBasalRate, TherapyState, Tier,
};

use super::crc::crc16_ok;
use super::io;
use super::link::{le_i32, Ctx, Halt, Step, WriteClass};
use crate::protocol::setting;
use crate::Characteristic;

const Y2K: u64 = 946_684_800;
const DAY: u64 = 86_400;
const WINDOW: u64 = 180;
const MINUTE: Duration = Duration::from_secs(60);
const QUARTER: Duration = Duration::from_secs(15 * 60);

pub(crate) fn milli(hundredths: i32) -> u32 {
    hundredths.max(0).unsigned_abs() * 10
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct System {
    pub(crate) suspended: bool,
    pub(crate) reservoir: Option<InsulinAmount>,
    pub(crate) battery: Option<Percent>,
    pub(crate) basal: BasalRate,
    pub(crate) percent: i32,
}

impl System {
    fn decode(bytes: &[u8]) -> Self {
        let reservoir = le_i32(bytes, 1);
        let suspended = bytes[0] == 3;
        Self {
            suspended,
            reservoir: (reservoir != -1).then(|| InsulinAmount::from_milliunits(milli(reservoir))),
            battery: (bytes[5] <= 5).then(|| Percent::new(u16::from(bytes[5]) * 20)),
            basal: BasalRate::from_milliunits_per_hour(milli(le_i32(bytes, 6))),
            percent: if suspended { 0 } else { le_i32(bytes, 10) },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BolusState {
    pub(crate) running: bool,
    pub(crate) delivered: InsulinAmount,
    pub(crate) total: InsulinAmount,
    pub(crate) extended_sequence: i32,
    pub(crate) extended_total: InsulinAmount,
    pub(crate) extended_minutes: i32,
}

impl BolusState {
    fn decode(bytes: &[u8], extended: bool) -> Step<Self> {
        let pick = |at| InsulinAmount::from_milliunits(milli(le_i32(bytes, at)));
        if !extended {
            let (delivered, total) = (pick(1), pick(5));
            return Ok(Self {
                running: running(bytes[0], delivered, total),
                delivered,
                total,
                ..Self::default()
            });
        }
        if bytes.len() != 44 || !crc16_ok(bytes) {
            return Err(Halt::Failed(Failure::Protocol));
        }
        let (delivered, total) = (pick(5), pick(9));
        Ok(Self {
            running: running(bytes[0], delivered, total),
            delivered,
            total,
            extended_sequence: le_i32(bytes, 14),
            extended_total: pick(22),
            extended_minutes: le_i32(bytes, 38),
        })
    }
}

fn running(status: u8, delivered: InsulinAmount, total: InsulinAmount) -> bool {
    matches!(status, 1 | 2) && total > InsulinAmount::ZERO && delivered != total
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Device {
    pub(crate) limits: Limits,
    pub(crate) long_percent: u16,
    pub(crate) long_minutes: u64,
    pub(crate) short_minutes: u64,
}

pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        reservoir_capacity: Some(InsulinAmount::from_milliunits(160_000)),
        max_profile_segments: 24,
        min_segment_duration: Duration::from_secs(3600),
        history: true,
        daily_totals: false,
        patch: false,
    }
}

fn rate(milli: u32) -> BasalRate {
    BasalRate::from_milliunits_per_hour(milli)
}

impl Device {
    pub(crate) fn new(step: u32, basal: (u32, u32), tbr: [u64; 4], extended: bool) -> Self {
        let amount = InsulinAmount::from_milliunits;
        let [long_percent, long_minutes, short_percent, short_minutes] = tbr;
        Self {
            limits: Limits {
                bolus: Bounds::uniform(amount(step), amount(25_000), amount(step)),
                basal: Bounds {
                    min: rate(basal.0),
                    max: rate(basal.1),
                    tiers: vec![
                        Tier {
                            from: rate(0),
                            step: rate(10),
                        },
                        Tier {
                            from: rate(1_000),
                            step: rate(20),
                        },
                        Tier {
                            from: rate(2_000),
                            step: rate(100),
                        },
                        Tier {
                            from: rate(15_000),
                            step: rate(500),
                        },
                    ],
                },
                temp_basal_percent: Some(Bounds::uniform(
                    Percent::new(0),
                    Percent::new(long_percent.max(short_percent) as u16),
                    Percent::new(1),
                )),
                temp_basal_absolute: None,
                temp_basal_duration: Bounds::uniform(
                    QUARTER,
                    MINUTE * long_minutes.max(short_minutes) as u32,
                    QUARTER,
                ),
                extended_bolus_duration: extended
                    .then(|| Bounds::uniform(QUARTER, MINUTE * 720, MINUTE)),
            },
            long_percent: long_percent as u16,
            long_minutes,
            short_minutes,
        }
    }

    pub(crate) fn defaults() -> Self {
        Self::new(50, (20, 40_000), [200, 240, 200, 240], false)
    }

    #[cfg(feature = "delivery")]
    pub(crate) fn allows_temp_basal(&self, percent: Percent, duration: Duration) -> bool {
        let max = if percent.get() <= self.long_percent {
            self.long_minutes
        } else {
            self.short_minutes
        };
        self.limits
            .allows_temp_basal(TempBasalRate::Percent(percent), duration)
            && duration <= MINUTE * max as u32
    }
}

impl Ctx {
    pub(crate) async fn system(&self) -> Step<System> {
        Ok(System::decode(
            &self.read_sized(Characteristic::GetSystemStatus, 14).await?,
        ))
    }

    pub(crate) async fn bolus_state(&self) -> Step<BolusState> {
        let extended = self.extended();
        BolusState::decode(
            &self.read_sized(Characteristic::GetBolusStatus, 13).await?,
            extended,
        )
    }

    pub(crate) fn extended(&self) -> bool {
        self.link.borrow().as_ref().is_some_and(|v| v.extended())
    }

    fn avoid_window(&self) -> bool {
        self.link.borrow().as_ref().is_some_and(|v| v.control < 1.3)
    }

    async fn setting_or(&self, id: u16, default: i32) -> Step<i32> {
        match self.read_setting(id).await {
            Ok(value) if value > 0 => Ok(value),
            Ok(_) | Err(Halt::Failed(Failure::Status(_))) => Ok(default),
            Err(other) => Err(other),
        }
    }

    pub(crate) async fn device(&self) -> Step<Device> {
        let step = self.setting_or(setting::BOLUS_STEP_SELECTION, 5).await?;
        let step = if step % 5 == 0 && step <= 100 {
            step
        } else {
            5
        };
        let min = self.setting_or(setting::BASAL_RATE_MIN, 2).await?;
        let max = self.setting_or(setting::BASAL_RATE_MAX, 4_000).await?;
        let mut tbr = [200, 240, 200, 240];
        for (value, id) in tbr.iter_mut().zip(setting::TBR_LONG_SCALE_MAX..) {
            *value = self.setting_or(id, *value as i32).await? as u64;
        }
        Ok(Device::new(
            milli(step),
            (milli(min), milli(max)),
            tbr,
            self.extended(),
        ))
    }

    pub(crate) async fn clock(&self) -> Step<u64> {
        let mut seconds = 0;
        for _ in 0..2 {
            let date = self.read_sized(Characteristic::SystemDate, 4).await?;
            let time = self.read_sized(Characteristic::SystemTime, 3).await?;
            let year = i64::from(u16::from_le_bytes([date[0], date[1]]));
            let days =
                days_from_civil(year, date[2], date[3]).ok_or(Halt::Failed(Failure::Protocol))?;
            let of_day = u64::from(time[0]) * 3600 + u64::from(time[1]) * 60 + u64::from(time[2]);
            seconds = days * DAY + of_day;
            if !(3..60).contains(&of_day) {
                break;
            }
        }
        Ok(seconds)
    }

    pub(crate) async fn set_clock(&self, local: u64) -> Step<()> {
        let of_day = local % DAY;
        let time = [of_day / 3600, of_day / 60 % 60, of_day % 60].map(|v| v as u8);
        let (year, month, day) = civil_from_days(local / DAY);
        let mut date = year.to_le_bytes().to_vec();
        date.extend([month, day]);
        self.write(Characteristic::SystemTime, WriteClass::Clock, &time)
            .await?;
        self.write(Characteristic::SystemDate, WriteClass::Clock, &date)
            .await
    }

    pub(crate) async fn wait_window(&self, always: bool, expiry: Option<Duration>) -> Step<()> {
        if !always && !self.avoid_window() {
            return Ok(());
        }
        let pos = self.clock().await? % WINDOW;
        let at = expiry.map_or(pos, |d| (pos + (d.as_secs() / 60 % WINDOW) * 60) % WINDOW);
        if !(6..WINDOW - 8).contains(&at) {
            io::sleep(&self.io, Duration::from_secs((WINDOW + 6 - at) % WINDOW)).await;
        }
        Ok(())
    }

    pub(crate) async fn status(&self, before: TherapyState) -> Step<PumpStatus> {
        let system = self.system().await?;
        let bolus = self.bolus_state().await?;
        let clock = self.clock().await?;
        let basal = match (system.percent, before.basal) {
            (100, _) => Basal::Scheduled,
            (percent, Basal::Temporary { rate, duration })
                if rate == TempBasalRate::Percent(Percent::new(percent as u16)) =>
            {
                Basal::Temporary { rate, duration }
            }
            _ => Basal::Unknown,
        };
        let extended = match (bolus.extended_sequence > 0, before.extended) {
            (false, _) => Extended::Idle,
            (true, running @ Extended::Running { .. }) => running,
            (true, _) => Extended::Running {
                amount: bolus.extended_total,
                duration: MINUTE * bolus.extended_minutes.max(0).unsigned_abs(),
            },
        };
        let therapy = TherapyState {
            mode: if system.suspended {
                PumpMode::Suspended
            } else {
                PumpMode::Running
            },
            basal: if system.suspended {
                Basal::Scheduled
            } else {
                basal
            },
            bolus: if bolus.running {
                Bolus::Immediate {
                    requested: bolus.total,
                    delivered: bolus.delivered,
                }
            } else {
                Bolus::Idle
            },
            extended,
        };
        Ok(PumpStatus {
            therapy,
            battery: system.battery,
            reservoir: system.reservoir,
            basal_rate: Some(system.basal),
            clock: Some(PumpTime::from_local_seconds(clock)),
            delivery_uncertain: false,
            alarms: vec![],
        })
    }
}

pub(crate) fn pump_seconds(y2k: u32) -> u64 {
    u64::from(y2k) + Y2K
}

fn days_from_civil(year: i64, month: u8, day: u8) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u64::try_from(era * 146_097 + doe - 719_468).ok()
}

fn civil_from_days(days: u64) -> (u16, u8, u8) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        (yoe + era * 400 + u64::from(month <= 2)) as u16,
        month as u8,
        day as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        for days in [0, 10_957, 18_993, 19_052, 20_000, 60_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(i64::from(y), m, d), Some(days));
        }
        assert_eq!(civil_from_days(18_993), (2022, 1, 1));
        assert_eq!(days_from_civil(2022, 13, 1), None);
        assert_eq!(days_from_civil(2022, 1, 0), None);
        assert_eq!(days_from_civil(1960, 1, 1), None);
    }
}
