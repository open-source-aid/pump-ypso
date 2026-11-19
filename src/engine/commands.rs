use alloc::boxed::Box;
use alloc::vec::Vec;
use core::time::Duration;

#[cfg(feature = "delivery")]
use pump_shared::TempBasalRate;
use pump_shared::{
    BasalProfile, BasalRate, BasalSegment, Command, DeviceInfo, Event, Failure, Outcome,
    PumpStatus, PumpTime, Refusal, Report, Sequence, TherapyState, Transition,
};

use super::crc::with_crc16;
use super::io;
use super::link::{safevar, unknown, Ctx, Halt, Step, WriteClass};
use super::pump::{capabilities, milli};
use crate::protocol::setting;
use crate::Characteristic;

pub(super) const VERIFY: usize = 4;
pub(super) const IMMEDIATE: u8 = 1;
pub(super) const SLOW: u8 = 2;
pub(super) const HOUR: u64 = 3600;
const DAY: u64 = 86_400;
const ALLOWANCE: u64 = 200;
const MIDNIGHT_MARGIN: u64 = 5;

pub(super) fn refused(refusal: Refusal) -> Outcome {
    Outcome::Refused(refusal)
}

impl Ctx {
    pub(crate) fn therapy(&self) -> TherapyState {
        self.io.borrow().therapy
    }

    pub(crate) fn apply(&self, transition: Transition) {
        let mut io = self.io.borrow_mut();
        let before = io.therapy;
        if io.therapy.apply(transition).is_ok() && io.therapy != before {
            let after = io.therapy;
            io.events.push_back(Event::TherapyChanged(after));
        }
    }

    pub(super) fn emit(&self, event: Event) {
        self.io.borrow_mut().events.push_back(event);
    }

    pub(super) fn bolus_payload(&self, hundredths: i32, minutes: i32, kind: u8) -> Vec<u8> {
        if !self.extended() {
            return safevar(hundredths);
        }
        let mut payload = hundredths.to_le_bytes().to_vec();
        payload.extend(minutes.to_le_bytes());
        payload.extend(0i32.to_le_bytes());
        payload.push(kind);
        with_crc16(payload)
    }

    async fn ready(&self) -> Step<Option<PumpStatus>> {
        if !self.connect().await? && self.ready.get() {
            return Ok(None);
        }
        let device = self.device().await?;
        self.emit(Event::DeviceInfo(Box::new(DeviceInfo {
            serial: alloc::format!("{}", self.session.serial),
            firmware: self.firmware(),
            variant: self.variant,
            limits: device.limits.clone(),
            capabilities: capabilities(),
        })));
        self.device.replace(device);
        let status = self.status(self.therapy()).await?;
        self.apply(Transition::Observed(status.therapy));
        self.ready.set(true);
        Ok(Some(status))
    }

    pub(crate) async fn run(&self, command: Command) -> Outcome {
        let result = match plan(command) {
            Ok(plan) => self.execute(plan).await,
            Err(refusal) => Ok(refused(refusal)),
        };
        match result {
            Ok(outcome) => outcome,
            Err(halt) => halt.into(),
        }
    }

    async fn execute(&self, plan: Plan) -> Step<Outcome> {
        let fresh = self.ready().await?;
        if let Err(refusal) = self.therapy().guard(&plan.command()) {
            return Ok(refused(refusal));
        }
        let report = match plan {
            Plan::Status => Report::Status(match fresh {
                Some(status) => status,
                None => {
                    let status = self.status(self.therapy()).await?;
                    self.apply(Transition::Observed(status.therapy));
                    status
                }
            }),
            Plan::ReadClock => Report::Clock(PumpTime::from_local_seconds(self.clock().await?)),
            Plan::SyncClock => return self.sync_clock().await,
            Plan::History(after) => Report::History(self.history(after).await?),
            Plan::ReadProfile => match self.active_profile().await?.and_then(|s| profile(&s)) {
                Some(profile) => Report::Profile(profile),
                None => return Ok(refused(Refusal::Unsupported)),
            },
            Plan::CancelTempBasal => return self.cancel_temp_basal().await,
            Plan::CancelExtended => return self.cancel_extended().await.map(|()| done()),
            #[cfg(feature = "delivery")]
            Plan::Bolus(amount) => return self.bolus(amount).await,
            #[cfg(feature = "delivery")]
            Plan::RateBolus(amount, duration) => return self.rate_bolus(amount, duration).await,
            #[cfg(feature = "delivery")]
            Plan::Extended(amount, duration) => return self.extended_bolus(amount, duration).await,
            #[cfg(feature = "delivery")]
            Plan::TempBasal(percent, duration) => return self.temp_basal(percent, duration).await,
            #[cfg(feature = "delivery")]
            Plan::TempBasalAbsolute(rate, duration) => {
                return self.absolute_temp_basal(rate, duration).await
            }
            #[cfg(feature = "delivery")]
            Plan::SetProfile(profile) => return self.set_profile(&profile).await,
        };
        Ok(Outcome::Completed(report))
    }

    async fn sync_clock(&self) -> Step<Outcome> {
        let Some(wall) = self.wall_millis() else {
            return Ok(Outcome::Failed(Failure::Host));
        };
        let offset = i64::from(self.utc_offset) * 1000;
        let pump = self.clock().await? as i64 * 1000;
        let local = wall as i64 + offset;
        if pump.abs_diff(local) < 5_000 && pump >= 1_735_689_600_000 {
            return Ok(done());
        }
        self.wait_window(false, None).await?;
        let wall = self.wall_millis().unwrap_or(wall);
        let mut boundary = (wall + ALLOWANCE).div_ceil(1000);
        let of_day = (boundary as i64 + i64::from(self.utc_offset)).rem_euclid(DAY as i64) as u64;
        if of_day >= DAY - MIDNIGHT_MARGIN {
            boundary += DAY - of_day + MIDNIGHT_MARGIN;
        }
        io::sleep(
            &self.io,
            Duration::from_millis(boundary * 1000 - ALLOWANCE - wall),
        )
        .await;
        let target = (boundary as i64 + i64::from(self.utc_offset)).max(0) as u64;
        self.set_clock(target).await?;
        Ok(done())
    }

    fn wall_millis(&self) -> Option<u64> {
        let io = self.io.borrow();
        io.wall.map(|(wall, at)| {
            wall.as_unix_millis() + io.now.as_millis().saturating_sub(at.as_millis())
        })
    }

    pub(super) async fn active_profile(&self) -> Step<Option<[i32; 24]>> {
        if self.read_setting(setting::BASAL_PROFILE_SELECTION).await? != 3 {
            return Ok(None);
        }
        let mut slots = [0; 24];
        for (hour, slot) in (0..).zip(slots.iter_mut()) {
            *slot = self.read_setting(setting::basal_a(hour)).await?.max(0);
        }
        Ok(Some(slots))
    }

    async fn cancel_temp_basal(&self) -> Step<Outcome> {
        self.write(
            Characteristic::StartStopTbr,
            WriteClass::Therapy,
            &tbr(100, 0),
        )
        .await?;
        self.verify_percent(100).await?;
        self.apply(Transition::TempBasalEnded);
        Ok(done())
    }

    pub(super) async fn verify_percent(&self, percent: i32) -> Step<()> {
        for _ in 0..VERIFY {
            if self.system().await.map_err(unknown)?.percent == percent {
                return Ok(());
            }
        }
        Err(Halt::Unknown)
    }

    pub(crate) async fn cancel_extended(&self) -> Step<()> {
        if !self.extended() || self.bolus_state().await?.extended_sequence <= 0 {
            return Ok(());
        }
        let payload = self.bolus_payload(0, 0, SLOW);
        self.write(
            Characteristic::StartStopBolus,
            WriteClass::Therapy,
            &payload,
        )
        .await?;
        if self.bolus_state().await.map_err(unknown)?.extended_sequence > 0 {
            return Err(Halt::Unknown);
        }
        self.apply(Transition::ExtendedEnded);
        Ok(())
    }

    pub(crate) async fn stop_bolus(&self) -> Outcome {
        let stopped = async {
            self.connect().await?;
            let payload = self.bolus_payload(0, 0, IMMEDIATE);
            let first = self
                .write(
                    Characteristic::StartStopBolus,
                    WriteClass::Therapy,
                    &payload,
                )
                .await;
            if first.is_ok() {
                self.apply(Transition::BolusEnded);
            }
            let second = self.cancel_extended().await;
            self.system().await?;
            first.and(second)
        };
        match stopped.await {
            Ok(()) => done(),
            Err(halt) => halt.into(),
        }
    }
}

pub(super) fn done() -> Outcome {
    Outcome::Completed(Report::Done)
}

pub(super) fn tbr(percent: i32, minutes: i32) -> Vec<u8> {
    [safevar(percent), safevar(minutes)].concat()
}

fn profile(slots: &[i32; 24]) -> Option<BasalProfile> {
    let segments = (0..)
        .zip(slots)
        .map(|(hour, slot)| BasalSegment {
            start: Duration::from_secs(hour * HOUR),
            rate: BasalRate::from_milliunits_per_hour(milli(*slot)),
        })
        .collect();
    BasalProfile::new(segments).ok()
}

enum Plan {
    Status,
    ReadClock,
    SyncClock,
    History(Sequence),
    ReadProfile,
    CancelTempBasal,
    CancelExtended,
    #[cfg(feature = "delivery")]
    Bolus(pump_shared::InsulinAmount),
    #[cfg(feature = "delivery")]
    RateBolus(pump_shared::InsulinAmount, Duration),
    #[cfg(feature = "delivery")]
    Extended(pump_shared::InsulinAmount, Duration),
    #[cfg(feature = "delivery")]
    TempBasal(pump_shared::Percent, Duration),
    #[cfg(feature = "delivery")]
    TempBasalAbsolute(pump_shared::BasalRate, Duration),
    #[cfg(feature = "delivery")]
    SetProfile(BasalProfile),
}

impl Plan {
    fn command(&self) -> Command {
        match self {
            #[cfg(feature = "delivery")]
            Self::Bolus(amount) => Command::Bolus { amount: *amount },
            #[cfg(feature = "delivery")]
            Self::RateBolus(amount, duration) => Command::RateBolus {
                amount: *amount,
                duration: *duration,
            },
            #[cfg(feature = "delivery")]
            Self::Extended(amount, duration) => Command::ExtendedBolus {
                amount: *amount,
                duration: *duration,
            },
            #[cfg(feature = "delivery")]
            Self::TempBasal(percent, duration) => Command::TempBasal {
                rate: TempBasalRate::Percent(*percent),
                duration: *duration,
            },
            #[cfg(feature = "delivery")]
            Self::TempBasalAbsolute(rate, duration) => Command::TempBasal {
                rate: TempBasalRate::Absolute(*rate),
                duration: *duration,
            },
            #[cfg(feature = "delivery")]
            Self::SetProfile(profile) => Command::SetBasalProfile {
                profile: profile.clone(),
            },
            _ => Command::Status,
        }
    }
}

fn plan(command: Command) -> Result<Plan, Refusal> {
    Ok(match command {
        Command::Status => Plan::Status,
        Command::ReadClock => Plan::ReadClock,
        Command::SyncClock => Plan::SyncClock,
        Command::ReadHistory { after } => Plan::History(after),
        Command::ReadBasalProfile => Plan::ReadProfile,
        Command::CancelTempBasal => Plan::CancelTempBasal,
        Command::TempBasal { rate, .. } if rate.is_neutral() => Plan::CancelTempBasal,
        Command::CancelExtendedBolus => Plan::CancelExtended,
        #[cfg(feature = "delivery")]
        Command::Bolus { amount } => Plan::Bolus(amount),
        #[cfg(feature = "delivery")]
        Command::RateBolus { amount, duration } => Plan::RateBolus(amount, duration),
        #[cfg(feature = "delivery")]
        Command::ExtendedBolus { amount, duration } => Plan::Extended(amount, duration),
        #[cfg(feature = "delivery")]
        Command::TempBasal {
            rate: TempBasalRate::Percent(percent),
            duration,
        } => Plan::TempBasal(percent, duration),
        #[cfg(feature = "delivery")]
        Command::TempBasal {
            rate: TempBasalRate::Absolute(rate),
            duration,
        } => Plan::TempBasalAbsolute(rate, duration),
        #[cfg(feature = "delivery")]
        Command::SetBasalProfile { profile } => Plan::SetProfile(profile),
        #[cfg(not(feature = "delivery"))]
        Command::Bolus { .. }
        | Command::RateBolus { .. }
        | Command::ExtendedBolus { .. }
        | Command::TempBasal { .. }
        | Command::SetBasalProfile { .. } => return Err(Refusal::NotBuilt),
        _ => return Err(Refusal::Unsupported),
    })
}
