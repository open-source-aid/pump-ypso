use alloc::vec;
use core::time::Duration;

#[cfg(feature = "diag")]
use pump_shared::Event;
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
const NO_BASAL_CAP: i32 = i32::MAX / 10;
const WINDOW: u64 = 180;
const MINUTE: Duration = Duration::from_secs(60);
const QUARTER: Duration = Duration::from_secs(15 * 60);

pub(crate) fn milli(hundredths: i32) -> u32 {
    hundredths.max(0).unsigned_abs() * 10
}

/// Lower-case hex of a frame, for the `diag` status dumps.
#[cfg(feature = "diag")]
fn diag_hex(bytes: &[u8]) -> alloc::string::String {
    use core::fmt::Write;
    let mut hex = alloc::string::String::new();
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct System {
    pub(crate) suspended: bool,
    pub(crate) reservoir: Option<InsulinAmount>,
    pub(crate) battery: Option<Percent>,
    pub(crate) basal: BasalRate,
    pub(crate) percent: i32,
    pub(crate) remaining: Option<i32>,
}

const SYSTEM_WITH_REMAINING: usize = 18;
const SYSTEM_WITH_CRC: usize = 20;

impl System {
    fn decode(bytes: &[u8]) -> Step<Self> {
        if bytes.len() >= SYSTEM_WITH_CRC && !crc16_ok(&bytes[..SYSTEM_WITH_CRC]) {
            return Err(Halt::Failed(Failure::Protocol));
        }
        let reservoir = le_i32(bytes, 1);
        let suspended = bytes[0] == 3;
        Ok(Self {
            suspended,
            reservoir: (reservoir != -1).then(|| InsulinAmount::from_milliunits(milli(reservoir))),
            battery: (bytes[5] <= 5).then(|| Percent::new(u16::from(bytes[5]) * 20)),
            basal: BasalRate::from_milliunits_per_hour(milli(le_i32(bytes, 6))),
            percent: if suspended { 0 } else { le_i32(bytes, 10) },
            remaining: (bytes.len() >= SYSTEM_WITH_REMAINING).then(|| le_i32(bytes, 14)),
        })
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
        let (delivered, total) = if pick(30) > InsulinAmount::ZERO && total == InsulinAmount::ZERO {
            (pick(26), pick(30))
        } else {
            (delivered, total)
        };
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
        let bytes = self.read_sized(Characteristic::GetSystemStatus, 14).await?;
        // Diagnostic (feature `diag`): the raw delivery-mode byte, for pinning an
        // unknown device-state value against hardware. Confirmed on a real pump:
        // 10 = running, 3 = stopped — carries no reservoir, battery or clock.
        #[cfg(feature = "diag")]
        self.emit(Event::Log {
            level: 1,
            line: alloc::format!(
                "ypso sys raw len={} bytes={}",
                bytes.len(),
                diag_hex(&bytes)
            ),
        });
        System::decode(&bytes)
    }

    pub(crate) async fn bolus_state(&self) -> Step<BolusState> {
        let extended = self.extended();
        let bytes = self.read_sized(Characteristic::GetBolusStatus, 13).await?;
        // Diagnostic (feature `diag`): the raw bolus-status frame plus which
        // layout it was read against (`ext` selects delivered@5/total@9 vs
        // delivered@1/total@5) and what that yields, to pin a "0u of 0u" on the
        // wire bytes rather than guess the offsets. No key material is logged.
        #[cfg(feature = "diag")]
        self.emit(Event::Log {
            level: 1,
            line: alloc::format!(
                "ypso bolus raw ext={extended} len={} bytes={}",
                bytes.len(),
                diag_hex(&bytes)
            ),
        });
        let state = BolusState::decode(&bytes, extended)?;
        #[cfg(feature = "diag")]
        self.emit(Event::Log {
            level: 1,
            line: alloc::format!(
                "ypso bolus decoded delivered={} total={} running={}",
                state.delivered.milliunits(),
                state.total.milliunits(),
                state.running
            ),
        });
        Ok(state)
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

    async fn basal_limit(&self, id: u16) -> Step<Option<i32>> {
        match self.read_setting(id).await {
            Ok(value) => Ok((value >= 0).then_some(value)),
            Err(Halt::Failed(Failure::Status(_))) => Ok(None),
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
        let cap = self.basal_limit(setting::BASAL_RATE_CAP).await?;
        let min = self.basal_limit(setting::BASAL_RATE_MIN).await?;
        let max = self.basal_limit(setting::BASAL_RATE_MAX).await?;
        let upper = [cap, max]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(NO_BASAL_CAP);
        let mut tbr = [200, 240, 200, 240];
        for (value, id) in tbr.iter_mut().zip(setting::TBR_LONG_SCALE_MAX..) {
            *value = self.setting_or(id, *value as i32).await? as u64;
        }
        Ok(Device::new(
            milli(step),
            (milli(min.unwrap_or(0)), milli(upper)),
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
            (percent, _) => match system.remaining {
                Some(remaining) => Basal::Temporary {
                    rate: TempBasalRate::Percent(Percent::new(percent as u16)),
                    duration: MINUTE * remaining.max(0).unsigned_abs(),
                },
                None => Basal::Unknown,
            },
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
    use crate::engine::crc::with_crc16;

    fn status_bytes(percent: i32, remaining: i32) -> alloc::vec::Vec<u8> {
        let mut bytes = alloc::vec![10u8];
        bytes.extend(1_000i32.to_le_bytes());
        bytes.push(4);
        bytes.extend(800i32.to_le_bytes());
        bytes.extend(percent.to_le_bytes());
        bytes.extend(remaining.to_le_bytes());
        bytes
    }

    #[test]
    fn system_status_decodes_remaining_and_checks_the_crc() {
        let bytes = status_bytes(150, 26);
        let system = System::decode(&bytes).unwrap();
        assert_eq!(system.percent, 150);
        assert_eq!(system.remaining, Some(26));
        assert!(!system.suspended);

        let sealed = with_crc16(bytes.clone());
        assert_eq!(System::decode(&sealed).unwrap().remaining, Some(26));

        let mut corrupt = sealed.clone();
        *corrupt.last_mut().unwrap() ^= 0xFF;
        assert_eq!(
            System::decode(&corrupt),
            Err(Halt::Failed(Failure::Protocol))
        );

        assert_eq!(System::decode(&bytes[..14]).unwrap().remaining, None);
    }

    fn combined_bytes(immediate_total: i32, injected2: i32, total2: i32) -> alloc::vec::Vec<u8> {
        let mut bytes = alloc::vec![1u8];
        for value in [1, 30, immediate_total] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.push(1);
        for value in [7, 50, 600, injected2, total2, 0, 60] {
            bytes.extend(value.to_le_bytes());
        }
        with_crc16(bytes)
    }

    #[test]
    fn a_combined_bolus_status_falls_back_to_the_second_immediate_pair() {
        let milli = InsulinAmount::from_milliunits;
        let taken = BolusState::decode(&combined_bytes(0, 100, 250), true).unwrap();
        assert_eq!(taken.delivered, milli(1_000));
        assert_eq!(taken.total, milli(2_500));
        assert!(taken.running);
        let kept = BolusState::decode(&combined_bytes(300, 100, 250), true).unwrap();
        assert_eq!(kept.delivered, milli(300));
        assert_eq!(kept.total, milli(3_000));
    }

    #[test]
    fn the_basal_blackout_is_gated_on_the_control_version() {
        use super::super::io::Shared;
        use super::super::link::Versions;
        use super::super::store::{tests::blob, Nonces, Session};
        let session = Session::decode(&blob(|_| {})).unwrap();
        let ctx = Ctx::new(
            Shared::default(),
            session,
            Nonces::resume([7; 16], None),
            0,
            "ypso",
        );
        let versions = |control| Versions {
            pump: 1.2,
            master: alloc::string::String::new(),
            history: 1.4,
            control,
        };
        assert!(!ctx.avoid_window());
        ctx.link.replace(Some(versions(1.2)));
        assert!(ctx.avoid_window());
        ctx.link.replace(Some(versions(1.3)));
        assert!(!ctx.avoid_window());
    }

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
