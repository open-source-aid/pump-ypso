use core::time::Duration;

use pump_shared::{
    Approximation, BasalProfile, BasalRate, Event, Failure, InsulinAmount, Outcome, Percent,
    Refusal, Report, TempBasalRate, Transition,
};

use super::commands::{done, refused, tbr, HOUR, IMMEDIATE, SLOW};
use super::io;
use super::link::{unknown, Ctx, Halt, Step, WriteClass};
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
    } else if delivered == InsulinAmount::ZERO {
        // A bolus we commanded, that history still reports as nothing. The record
        // has not landed yet — `GetBolusStatus` clears the instant delivery ends,
        // so the live status is already empty and history lags it. Reporting a
        // definitive zero here would drop delivered insulin from the caller's IOB,
        // the dangerous direction, so say the amount is unknown and let the caller
        // reconcile it from history rather than trust a zero.
        Outcome::Unknown
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
/// The extended-bolus amount crosses the wire in hundredths of a unit (0.01 U).
/// The firmware range-checks it and never quantises it to the pump's configured
/// bolus step (verified against `BLL_DoseSvc_StartSlowBolus`), so the plan rounds
/// the emulated amount to this native 0.01 U grid rather than the ~10x coarser
/// step grid, which is what let arbitrary temp-basal rates fall outside
/// tolerance and be refused.
const EXTENDED_STEP: u32 = 10;
/// The fail-safe ceiling, in minutes, on the 0% TBR that an absolute temp basal
/// lays down to suspend scheduled basal. A host re-issues well within it, so it
/// only bites when the host or link falls silent — then scheduled basal resumes
/// within this window instead of staying suspended for hours. Capped here and
/// lowered further by the pump's own short-TBR limit (`TBR_SHORT_DURATION_MAX`).
const FALLBACK_SUSPEND: u64 = 30;
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
            // Skip a cleared frame's zero: the status empties the instant delivery
            // ends, and a spurious zero would flicker the running total to nothing
            // just as the bolus completes.
            if state.delivered != InsulinAmount::ZERO {
                self.apply(Transition::BolusProgressed {
                    delivered: state.delivered,
                });
                self.emit(Event::Progress {
                    id: self.current.get(),
                    delivered: state.delivered,
                    requested: amount,
                });
            }
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
        // The completed records lag the live status, which clears the instant
        // delivery ends, so wait briefly for history to show the dose before
        // concluding. A dose that lands here reports its real amount; only a link
        // that stays silent (the read errors) or a dose history never shows falls
        // through to Unknown — the one outcome a host must not auto-retry.
        let mut delivered;
        let mut tries = SETTLE;
        loop {
            delivered = self.replay(start).await?.0;
            tries = tries.saturating_sub(1);
            if delivered != InsulinAmount::ZERO || tries == 0 {
                break;
            }
            io::sleep(&self.io, POLL).await;
        }
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
            // Skip a cleared frame's zero: the status empties the instant delivery
            // ends, and a spurious zero would flicker the running total to nothing
            // just as the bolus completes.
            if state.delivered != InsulinAmount::ZERO {
                self.apply(Transition::BolusProgressed {
                    delivered: state.delivered,
                });
                self.emit(Event::Progress {
                    id: self.current.get(),
                    delivered: state.delivered,
                    requested: amount,
                });
            }
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
        // The firmware range-checks an extended-bolus amount (0.10 U .. cap) and
        // the duration, and does not snap the amount to the bolus step, so the
        // amount is validated on range alone — matching the pump — rather than
        // through `allows_extended_bolus`, whose step-grid check would reject the
        // 0.01 U-resolution amounts the plan now produces.
        let allowed = {
            let limits = &self.device.borrow().limits;
            let mu = amount.milliunits();
            mu >= MIN_EXTENDED
                && mu <= limits.bolus.max.milliunits()
                && limits
                    .extended_bolus_duration
                    .as_ref()
                    .is_some_and(|b| b.allows(duration))
        };
        if !allowed {
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

    /// Set a native percent TBR for `duration`. The reported duration is exactly
    /// what is programmed on the pump — never the host's original request — so a
    /// capped absolute temp basal does not leave the host modelling a longer dose
    /// than the pump will deliver (which, for a rate below the scheduled basal,
    /// would read IOB low and oversize the next dose if the host went silent).
    async fn set_percent_tbr(&self, percent: Percent, duration: Duration) -> Step<()> {
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
        Ok(())
    }

    pub(super) async fn absolute_temp_basal(
        &self,
        requested: BasalRate,
        duration: Duration,
    ) -> Step<Outcome> {
        let request = u64::from(requested.milliunits_per_hour());
        // Fail-safe: scheduled basal is suppressed for at most this window, so a
        // host that falls silent leaves it off for minutes, not hours. Clamped in
        // the driver and lowered further by the pump's own short-TBR limit.
        let fail_safe = self.device.borrow().short_minutes.min(FALLBACK_SUSPEND);
        let minutes = (duration.as_secs() / 60).min(fail_safe);
        if request == 0 {
            // Suspend: a native 0% TBR, no extended bolus and no firmware gate.
            self.set_percent_tbr(Percent::new(0), Duration::from_secs(minutes * 60))
                .await?;
            return Ok(done());
        }
        if !self.extended() {
            return Ok(refused(Refusal::Unsupported));
        }
        let (amount, window) = self.plan_extended(requested, minutes);
        let achieved = u64::from(amount.milliunits()).saturating_mul(60) / window;
        // The 0% TBR and the extended bolus share ONE window, so a silent host
        // lapses both together — scheduled basal never resumes under a running
        // bolus — and the suspension never outlasts the fail-safe window. A rate
        // whose plan would need a longer window than the fail-safe allows is
        // refused rather than delivered with the two diverging.
        if amount.milliunits() >= MIN_EXTENDED
            && window <= fail_safe
            && within_tolerance(request, achieved)
        {
            let span = Duration::from_secs(window * 60);
            self.set_percent_tbr(Percent::new(0), span).await?;
            self.cancel_extended().await?;
            self.extended_bolus(amount, span).await?;
            // This extended bolus stands in for the temp basal: a later
            // CancelTempBasal must stop it, not just lift the 0% TBR.
            self.emulated_temp_basal.set(true);
            return Ok(done());
        }
        Ok(refused(Refusal::OutOfLimits))
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
        let max_mu = bounds.max.milliunits();
        let ideal = u32::try_from(over(window)).unwrap_or(u32::MAX).min(max_mu);
        let rounded = (ideal + EXTENDED_STEP / 2) / EXTENDED_STEP * EXTENDED_STEP;
        let amount = InsulinAmount::from_milliunits(rounded.clamp(MIN_EXTENDED, max_mu));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bolus_history_cannot_confirm_is_unknown_not_a_definitive_zero() {
        let u = InsulinAmount::from_milliunits;
        let d = Duration::from_secs(180);
        // Delivered exactly what was asked: a clean completion.
        assert_eq!(
            completed_or(u(2_000), u(2_000), d),
            Outcome::Completed(Report::Delivered(u(2_000)))
        );
        // Partially delivered: the real amount, approximated.
        assert!(matches!(
            completed_or(u(2_000), u(1_500), d),
            Outcome::Approximated(Approximation::Bolus { delivered, .. }) if delivered == u(1_500)
        ));
        // Commanded, but history still shows nothing — the status cleared before
        // the record landed. This must be Unknown, never a definitive zero, so the
        // delivered insulin is reconciled from history rather than dropped from IOB.
        assert_eq!(
            completed_or(u(2_000), InsulinAmount::ZERO, d),
            Outcome::Unknown
        );
        // A genuine zero request that delivered zero is a clean completion.
        assert_eq!(
            completed_or(InsulinAmount::ZERO, InsulinAmount::ZERO, d),
            Outcome::Completed(Report::Delivered(InsulinAmount::ZERO))
        );
    }
}
