# Rate-controlled bolus by micro-bolus sequence (DRAFT, not yet implemented)

The YpsoPump has no API to set a delivery rate: a BLE client can only start an immediate bolus or the pump's own extended bolus (fixed internal profile). To deliver a chosen total over a chosen duration at a chosen rate, emulate it as a sequence of small immediate boluses spaced in time, closed by a clock-offset marker that records the sequence in the history for verification and recovery.

> Safety: an emulated sequence is many real boluses. A crash, disconnect or stop leaves part of it delivered. Recovery reads the history to learn exactly what was delivered before continuing or reporting. Anything that moves insulin stays behind the `delivery` feature; the stop path is never gated.

## Pump facts (sources)
- One immediate bolus step = setting 3 (hundredths of a unit, multiple of 5, 5..100). 6 motor steps = 0.01 U (firmware `MOV_GetDeliveredDoseUnits`, `round(steps/6)`).
- Delivery is not constant-rate: the motor controller (`test.bin`, `motor_compute_accel_profile` 0x7330) ramps. Observed over BLE (Rockit, hardware): ~87 ms per hundredth for an immediate bolus; peak ~12 U/min TO CONFIRM on the controller/bench.
- Between two boluses the engine already waits the basal blackout window and a sub-bolus delay (Rockit `timing.rs`, ~2.5 s). A micro-bolus cannot be sent faster than `dt_min = t_exec(step) + blackout + ble_round_trip`.
- A clock write records TIME_SET (type 13) / DATE_SET (type 12) in the event log, decoded as `HistoryEvent::ClockChanged { previous }`. The entry carries the previous clock, so one backward write encodes a delta directly.

## Scheduling a rate
Given `total` (hundredths), `duration` (s), and the pump `step` (hundredths):
- `n = round(total / step)` micro-boluses of `step` each (last one carries the remainder if `total` is not a multiple of `step`; a remainder below the minimum bolus is merged into the previous step).
- Target spacing `dt = duration / n`. Effective rate `r = step / dt`.
- `dt` is clamped to `>= dt_min`. If the requested rate needs `dt < dt_min`, the rate is unreachable at this step: either coarsen (more units per micro-bolus) or refuse with `OutOfLimits`. `r_max = step / dt_min`.
- Each tick: send `Bolus { step }`, then wait until the next scheduled instant (not a fixed delay — recompute from the sequence start so drift does not accumulate).

## End-of-sequence marker (clock CRC)
After the last micro-bolus:
1. Compute `c = (total hundredths) mod 59` (59 prime; keeps the offset <= 58 s, bounded regardless of n).
2. Write the clock back by `c` seconds, then write it forward to the true time. Two TIME_SET entries bracket the value; the first carries `previous - now = c`.
3. Never let either write cross midnight (reuse the sync-clock midnight margin). Stay within the same day so no DATE_SET / daily-total side effect fires.

A backward-then-forward pair of a few seconds is an improbable natural event, so it is a reliable sequence delimiter. Optionally a second marker encodes `n mod 59` as well as the total.

## Reading back and verifying
- The sequence is the immediate-bolus entries between the sequence start and the marker.
- Check `(sum of delivered hundredths) mod 59 == c`. A mismatch means a bolus was missed, duplicated or misplaced in the history.

## Recovery after interruption
- The final marker is written only on completion. Its absence means the sequence is incomplete.
- On reconnect: read the history, sum the micro-boluses delivered since the sequence start, subtract from the target, and either resume the remaining schedule or report `Outcome` with the exact delivered amount. Never re-deliver an already-recorded micro-bolus.

## To confirm on the bench
- Peak/effective delivery rate and `t_exec(step)` on the real pump and the controller.
- `dt_min` in practice (blackout + sub-bolus delay + BLE round trip).
- That a backward clock write of a few seconds within the same day produces exactly two TIME_SET entries and no other side effect (no alarm, no daily total).
- Whether to encode total, count, or both in the marker.

## Decisions (owner, 2026-11-19)

- **Micro-bolus is for boluses only, never for a temp basal.** A scheduled micro-bolus sequence for a basal would restart the motor almost continuously (a new TBR every AID cycle), draining the battery, wearing the mechanism and hurting at every step. A bolus is one-off, and spreading it as micro-boluses also reduces pain.
- **Temp basal at an absolute rate** is handled without micro-bolus: percent of the current profile when reachable, else the boost (pump max TBR + the pump's native extended bolus), else `Outcome::Approximated` carrying the closest reachable rate when it is within a small tolerance of the request (e.g. 5%), or `Refused(OutOfLimits)` when the gap is larger — never silently under-dose a basal (owner, 2026-11-19).
- **The app always asks for an absolute rate**; the library does the whole calculation (profile lookup, strategy choice, scheduling) and never exposes a percent.
- **`Outcome::Approximated`** (a new outcome, distinct from `Completed`) reports what was actually programmed (rate or total) whenever the result is not exactly the request, for both temp basal and rate-controlled bolus.

## Bounding and testing (safety)

Every calculation here is dose-affecting, so:
- All arithmetic is saturating or checked; no silent overflow/underflow. `n`, `dt`, the offset `c`, the per-tick amount and the running total are each bounded, and a value outside the pump grid or limits is refused, never rounded.
- Exhaustive tests: min/max/zero, total not a multiple of the step, remainder below the minimum bolus, `dt < dt_min` (rate unreachable), profile rate zero, the marker near midnight, `c` for a total that is and is not a multiple of 59.
- Recovery tested with a crash/disconnect at every step of a sequence; the reported dose must equal exactly what the history records.
- A `YpsoSim` case for every path and every pump status code; 100% coverage kept.

