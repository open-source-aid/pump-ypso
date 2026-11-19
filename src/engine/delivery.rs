use core::time::Duration;

use pump_shared::{
    BasalProfile, BasalRate, Event, Failure, InsulinAmount, Outcome, Percent, Refusal, Report,
    TempBasalRate, Transition,
};

use super::commands::{done, refused, tbr, HOUR, IMMEDIATE, SLOW};
use super::io;
use super::link::{unknown, Ctx, Halt, Step, WriteClass};
use crate::protocol::setting;
use crate::Characteristic;

const POLL: Duration = Duration::from_secs(2);
const POLLS: usize = 450;
const SETTLE: usize = 5;
const COMMAND_FAILED: u8 = 0x87;
const TBR_RUNNING: u8 = 0xB0;

fn hundredths(amount: InsulinAmount) -> i32 {
    (amount.milliunits() / 10) as i32
}

fn minutes(duration: Duration) -> i32 {
    (duration.as_secs() / 60) as i32
}

impl Ctx {
    async fn dose(&self, characteristic: Characteristic, payload: &[u8]) -> Step<()> {
        match self
            .write(characteristic, WriteClass::Therapy, payload)
            .await
        {
            Err(halt @ Halt::Failed(Failure::Status(COMMAND_FAILED))) => {
                Err(self.why_refused(halt).await)
            }
            Err(Halt::Failed(Failure::Status(TBR_RUNNING))) => Err(Halt::Refused(Refusal::Busy)),
            other => other,
        }
    }

    async fn why_refused(&self, halt: Halt) -> Halt {
        match self.system().await {
            Ok(system) if system.suspended => Halt::Refused(Refusal::NotRunning),
            _ => halt,
        }
    }

    pub(super) async fn bolus(&self, amount: InsulinAmount) -> Step<Outcome> {
        let allowed = self.device.borrow().limits.bolus.allows(amount);
        if !allowed {
            return Ok(refused(Refusal::OutOfLimits));
        }
        let snapshot = self.newest_sequence().await?;
        self.wait_window(true, None).await?;
        let payload = self.bolus_payload(hundredths(amount), 0, IMMEDIATE);
        self.dose(Characteristic::StartStopBolus, &payload).await?;
        self.apply(Transition::BolusStarted { requested: amount });
        for _ in 0..POLLS {
            io::sleep(&self.io, POLL).await;
            let Ok(state) = self.bolus_state().await else {
                break;
            };
            self.apply(Transition::BolusProgressed {
                delivered: state.delivered,
            });
            self.emit(Event::Progress {
                id: self.current.get(),
                delivered: state.delivered,
                requested: amount,
            });
            if !state.running {
                break;
            }
        }
        self.apply(Transition::BolusEnded);
        for _ in 0..SETTLE {
            if let Some(delivered) = self.delivered_since(snapshot).await.unwrap_or(None) {
                return Ok(Outcome::Completed(Report::Delivered(delivered)));
            }
            io::sleep(&self.io, POLL).await;
        }
        Ok(Outcome::Unknown)
    }

    pub(super) async fn extended_bolus(
        &self,
        amount: InsulinAmount,
        duration: Duration,
    ) -> Step<Outcome> {
        if !self.extended() {
            return Ok(refused(Refusal::Unsupported));
        }
        let allowed = self
            .device
            .borrow()
            .limits
            .allows_extended_bolus(amount, duration);
        if !allowed || amount.milliunits() < 100 {
            return Ok(refused(Refusal::OutOfLimits));
        }
        self.wait_window(true, None).await?;
        let payload = self.bolus_payload(hundredths(amount), minutes(duration), SLOW);
        self.dose(Characteristic::StartStopBolus, &payload).await?;
        let state = self.bolus_state().await.map_err(unknown)?;
        if state.extended_sequence <= 0 || state.extended_total != amount {
            return Err(Halt::Unknown);
        }
        self.apply(Transition::ExtendedStarted { amount, duration });
        Ok(done())
    }

    pub(super) async fn temp_basal(&self, percent: Percent, duration: Duration) -> Step<Outcome> {
        let allowed = self.device.borrow().allows_temp_basal(percent, duration);
        if !allowed {
            return Ok(refused(Refusal::OutOfLimits));
        }
        self.wait_window(false, None).await?;
        self.wait_window(true, Some(duration)).await?;
        let cleared = self.system().await?.percent != 100;
        if cleared {
            self.write(
                Characteristic::StartStopTbr,
                WriteClass::Therapy,
                &tbr(100, 0),
            )
            .await?;
            self.apply(Transition::TempBasalEnded);
        }
        let value = i32::from(percent.get());
        let after = |halt| if cleared { unknown(halt) } else { halt };
        self.dose(Characteristic::StartStopTbr, &tbr(value, minutes(duration)))
            .await
            .map_err(after)?;
        self.verify_percent(value).await.map_err(after)?;
        self.apply(Transition::TempBasalStarted {
            rate: TempBasalRate::Percent(percent),
            duration,
        });
        Ok(done())
    }

    pub(super) async fn set_profile(&self, profile: &BasalProfile) -> Step<Outcome> {
        let Some(wanted) = self.slots(profile) else {
            return Ok(refused(Refusal::OutOfLimits));
        };
        let Some(current) = self.active_profile().await? else {
            return Ok(refused(Refusal::Unsupported));
        };
        for (hour, (want, have)) in (0..).zip(wanted.iter().zip(current)) {
            if *want != have {
                self.wait_window(false, None).await?;
                self.write_setting(setting::basal_a(hour), *want)
                    .await
                    .map_err(unknown)?;
            }
        }
        match self.active_profile().await.map_err(unknown)? {
            Some(read) if read == wanted => Ok(done()),
            _ => Err(Halt::Unknown),
        }
    }

    fn slots(&self, profile: &BasalProfile) -> Option<[i32; 24]> {
        let bounds = &self.device.borrow().limits.basal;
        let mut slots = [0; 24];
        for (hour, slot) in (0..24u64).zip(slots.iter_mut()) {
            let rate = profile.rate_at(Duration::from_secs(hour * HOUR));
            if rate != BasalRate::ZERO && !bounds.allows(rate) {
                return None;
            }
            *slot = (rate.milliunits_per_hour() / 10) as i32;
        }
        profile
            .segments()
            .iter()
            .all(|s| s.start.as_secs() % HOUR == 0 && s.start.subsec_nanos() == 0)
            .then_some(slots)
    }
}
