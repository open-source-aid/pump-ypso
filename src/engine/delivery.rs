use core::time::Duration;

use pump_shared::{
    Approximation, BasalProfile, BasalRate, Event, Failure, InsulinAmount, Outcome, Percent,
    Refusal, Report, TempBasalRate, Transition,
};

use super::commands::{done, refused, tbr, HOUR, IMMEDIATE, SLOW};
use super::io;
use super::link::{unknown, Ctx, Halt, Step, WriteClass};
use super::pump::milli;
use crate::protocol::setting;
use crate::Characteristic;

const POLL: Duration = Duration::from_secs(2);
const POLLS: usize = 450;
const SETTLE: usize = 5;
const DAY: u64 = HOUR * 24;
const MINUTE: u64 = 60;
const DT_MIN: Duration = Duration::from_secs(18);
const MAX_MICRO: u64 = 256;
const MARKER_MOD: u64 = 59;

struct Schedule {
    ticks: u64,
    per_tick: u64,
    steps: u64,
    step: u32,
    dt_ms: u64,
}

impl Schedule {
    fn amount_at(&self, tick: u64) -> InsulinAmount {
        let done = tick.saturating_mul(self.per_tick);
        let steps = self.steps.saturating_sub(done).min(self.per_tick);
        InsulinAmount::from_milliunits((steps as u32).saturating_mul(self.step))
    }
}

fn plan_micro(
    bolus: &pump_shared::Bounds<InsulinAmount>,
    amount: InsulinAmount,
    duration: Duration,
) -> Option<Schedule> {
    let step = bolus.min.milliunits();
    if step == 0 || amount < bolus.min || amount > bolus.max {
        return None;
    }
    let steps = u64::from(bolus.nearest(amount).unwrap_or(bolus.min).milliunits() / step);
    let cap = duration.as_secs() / DT_MIN.as_secs();
    let (ticks, per_tick) = if steps == 1 || steps <= cap {
        (steps, 1)
    } else if cap >= 2 {
        let per_tick = steps.div_ceil(cap);
        (steps.div_ceil(per_tick), per_tick)
    } else {
        return None;
    };
    if ticks > MAX_MICRO {
        return None;
    }
    let dt_ms = duration.as_secs().saturating_mul(1_000) / ticks;
    Some(Schedule {
        ticks,
        per_tick,
        steps,
        step,
        dt_ms,
    })
}

fn completed_or(requested: InsulinAmount, delivered: InsulinAmount, duration: Duration) -> Outcome {
    if delivered == requested {
        Outcome::Completed(Report::Delivered(delivered))
    } else {
        Outcome::Approximated(Approximation::Bolus {
            requested,
            delivered,
            duration,
        })
    }
}
const COMMAND_FAILED: u8 = 0x87;
const TBR_RUNNING: u8 = 0xB0;
const MIN_EXTENDED: u32 = 100;
const ZERO_RATE_EXTENSION: u64 = 15;
const SLOW_STEP: u64 = 15;
const STRETCH_LIMIT: u64 = 60;
const STEP_ITERS: usize = 64;
const TOLERANCE: u64 = 20;

fn hundredths(amount: InsulinAmount) -> i32 {
    (amount.milliunits() / 10) as i32
}

fn minutes(duration: Duration) -> i32 {
    (duration.as_secs() / 60) as i32
}

fn within_tolerance(requested: u64, achieved: u64) -> bool {
    requested.abs_diff(achieved).saturating_mul(TOLERANCE) <= requested
}

fn basal_approximation(requested: BasalRate, achieved: BasalRate) -> Outcome {
    Outcome::Approximated(Approximation::Basal {
        requested,
        achieved,
    })
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

    pub(super) async fn rate_bolus(
        &self,
        amount: InsulinAmount,
        duration: Duration,
    ) -> Step<Outcome> {
        let Some(schedule) = plan_micro(&self.device.borrow().limits.bolus, amount, duration)
        else {
            return Ok(refused(Refusal::OutOfLimits));
        };
        let start = self.newest_sequence().await?;
        let origin = self.io.borrow().now.as_millis();
        for tick in 0..schedule.ticks {
            let due = origin.saturating_add(tick.saturating_mul(schedule.dt_ms));
            let now = self.io.borrow().now.as_millis();
            if due > now {
                io::sleep(&self.io, Duration::from_millis(due - now)).await;
            }
            if self.micro(schedule.amount_at(tick)).await.is_err() {
                return Ok(match self.replay(start).await {
                    Ok((delivered, _)) => completed_or(amount, delivered, duration),
                    Err(_) => Outcome::Unknown,
                });
            }
        }
        let (delivered, _) = self.replay(start).await?;
        let offset = u64::from(delivered.milliunits() / 10) % MARKER_MOD;
        let marked = self.write_marker(offset).await?;
        let (confirmed, marker) = self.replay(start).await?;
        if marked && confirmed == delivered && marker == Some(offset) {
            Ok(completed_or(amount, confirmed, duration))
        } else {
            Ok(Outcome::Unknown)
        }
    }

    async fn micro(&self, amount: InsulinAmount) -> Step<()> {
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
        Ok(())
    }

    async fn write_marker(&self, offset: u64) -> Step<bool> {
        let now = self.clock().await?;
        let now = if now % MINUTE < offset {
            io::sleep(&self.io, Duration::from_secs(offset - now % MINUTE)).await;
            self.clock().await?
        } else {
            now
        };
        for target in [now - offset, now] {
            let of_day = target % DAY;
            let time = [of_day / 3600, of_day / 60 % 60, of_day % 60].map(|v| v as u8);
            if self
                .write(Characteristic::SystemTime, WriteClass::Clock, &time)
                .await
                .is_err()
            {
                return Ok(false);
            }
        }
        Ok(true)
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
        if !allowed || amount.milliunits() < MIN_EXTENDED {
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
        self.set_percent_tbr(percent, duration, duration).await?;
        Ok(done())
    }

    async fn set_percent_tbr(
        &self,
        percent: Percent,
        pump_duration: Duration,
        reported: Duration,
    ) -> Step<()> {
        self.wait_window(false, None).await?;
        self.wait_window(true, Some(pump_duration)).await?;
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
        self.dose(
            Characteristic::StartStopTbr,
            &tbr(value, minutes(pump_duration)),
        )
        .await
        .map_err(after)?;
        self.verify_percent(value).await.map_err(after)?;
        self.apply(Transition::TempBasalStarted {
            rate: TempBasalRate::Percent(percent),
            duration: reported,
        });
        Ok(())
    }

    pub(super) async fn absolute_temp_basal(
        &self,
        requested: BasalRate,
        duration: Duration,
    ) -> Step<Outcome> {
        let request = u64::from(requested.milliunits_per_hour());
        if request == 0 {
            return self.temp_basal(Percent::new(0), duration).await;
        }
        if let Some(scheduled) = self.scheduled_rate().await? {
            if let Some(ideal) = requested.percent_of(scheduled) {
                let cap = self
                    .device
                    .borrow()
                    .limits
                    .temp_basal_percent
                    .as_ref()
                    .map_or(0, |bounds| bounds.max.get());
                let percent = ideal.get().min(cap);
                let achieved = u64::from(scheduled.milliunits_per_hour())
                    .saturating_mul(u64::from(percent))
                    / 100;
                if within_tolerance(request, achieved) {
                    let rate = BasalRate::from_milliunits_per_hour(achieved as u32);
                    return Ok(
                        match self.temp_basal(Percent::new(percent), duration).await? {
                            Outcome::Completed(_) if achieved == request => done(),
                            Outcome::Completed(_) => basal_approximation(requested, rate),
                            other => other,
                        },
                    );
                }
            }
        }
        if self.extended() {
            let minutes = duration.as_secs() / 60;
            let (amount, window) = self.plan_extended(requested, minutes);
            let achieved = u64::from(amount.milliunits()).saturating_mul(60) / window;
            if amount.milliunits() >= MIN_EXTENDED && within_tolerance(request, achieved) {
                let ceiling = self.device.borrow().long_minutes;
                let pump = (minutes + ZERO_RATE_EXTENSION).min(ceiling);
                self.set_percent_tbr(Percent::new(0), Duration::from_secs(pump * 60), duration)
                    .await?;
                self.cancel_extended().await?;
                self.extended_bolus(amount, Duration::from_secs(window * 60))
                    .await?;
                let rate = BasalRate::from_milliunits_per_hour(achieved as u32);
                return Ok(basal_approximation(requested, rate));
            }
        }
        Ok(refused(Refusal::OutOfLimits))
    }

    async fn scheduled_rate(&self) -> Step<Option<BasalRate>> {
        let Some(slots) = self.active_profile().await? else {
            return Ok(None);
        };
        let hour = (self.clock().await? / HOUR % 24) as usize;
        Ok(Some(BasalRate::from_milliunits_per_hour(milli(
            slots[hour],
        ))))
    }

    fn plan_extended(&self, requested: BasalRate, minutes: u64) -> (InsulinAmount, u64) {
        let bounds = self.device.borrow().limits.bolus.clone();
        let request = u64::from(requested.milliunits_per_hour());
        let over = |window: u64| request.saturating_mul(window) / 60;
        let mut window = minutes.max(SLOW_STEP);
        while over(window) < u64::from(MIN_EXTENDED) && window < STRETCH_LIMIT {
            window += SLOW_STEP;
        }
        let ceiling = u64::from(bounds.max.milliunits());
        let mut steps = 0;
        while over(window) > ceiling && window > SLOW_STEP && steps < STEP_ITERS {
            window -= SLOW_STEP;
            steps += 1;
        }
        let target =
            InsulinAmount::from_milliunits(u32::try_from(over(window)).unwrap_or(u32::MAX));
        let amount = bounds.nearest(target).unwrap_or(bounds.min);
        (amount, window)
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
