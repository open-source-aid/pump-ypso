# YpsoPump behaviours to confirm on the bench

What the simulator implements from the spec, Rockit and the firmware verification but that no source settles. Each item names the bench test that settles it. Tick it and update the simulator when done.

| # | Point | Simulator today | Bench test |
|---|---|---|---|
| 1 | ControlNotify payload during a bolus | not implemented | subscribe to ControlNotify during a bolus and capture the frames |
| 2 | Trailer of bolus status and system status: genCRC16 or u16 length | genCRC16 (Rockit) | capture both decrypted replies on V05.02.03 and test the last 2 bytes against genCRC16 and against the length |
| 3 | What resets the counters and bumps the epoch | only a new pairing key; a reboot keeps counting | read the trailer (reboot, counter) after a reboot, then after a pairing |
| 4 | System status length | 18 bytes OPN, 20 encrypted | capture it with and without a temp basal running |
| 5 | History entry count: entries held or log capacity | entries held | read the count, then walk until 0x84 |
| 6 | Record of a bolus stopped remotely vs on the pump | type 2 with the partial amount; 29 for pump or alarm stops | stop a bolus remotely and from the pump, read both records |
| 7 | Extended bolus cancel record | 30/31 with delivered amount and elapsed minutes | BENCH-ABORT-CHECK run B |
| 8 | Temp basal records | type 10 with minutes run on cancel, 32 on a pump stop | BENCH-ABORT-CHECK run A |
| 9 | Bolus status after the end | status 0x04, fields kept, extended fields cleared | read GetBolusStatus after completion and after a stop |
| 10 | OPN delivery rate | 87 ms per hundredth, as encrypted pumps | time a 5 U bolus on an OPN pump |
| 11 | Refusal codes inferred from "0x82 is out of range" | 0x82 for limits, 0x81 for bad CRC or length, 0x87 for a second extended bolus; accepts a stop with nothing running | send each case and record the code |
| 12 | Pump stop and alarm effects | mode value 3 when stopped, therapy aborted, alarms active until acknowledged, zero alarm payloads | stop from the pump menu, trigger a test occlusion, read setting 2 and both logs |
| 13 | Basal delivery model | 3-minute pulses with carry; status basal rate is the profile slot before the temp basal | watch the reservoir; read the status with a temp basal running |
| 14 | Off-step bolus | accepted | send 0.07 U with the step at 0.10 U |
| 15 | Setting writes | any id accepted; profile switch logs nothing; 112/113 log 33/34 (firmware); 3 logs 5 on any write, remote or `change_setting` (the firmware names the writer, not its callers) | write a non-basal setting and an out-of-range basal rate |
| 16 | Offline pairing details | Rockit's current choices; fixed challenge; no "challenge in progress" 0x88; 0x81 without key at the exchange write; 0x88/0x89 also block sealed operations | one offline pairing: two offer reads, the blob write, sealed reads at 0, 4 and 6 minutes |
| 17 | `key_agree` result | raw X25519 | contract decision in pump-shared |
| 18 | Access before the password outside settings | 0x08 for everything else; one full-access password | probe each service before the password and after each key |
| 19 | Encrypted-class pump with no session | writes answer 0x8A | write a clear SettingID to an unpaired pump |
| 20 | Link security knobs | apply to everything except version reads | read each characteristic on an unbonded link |
| 21 | Persistence across reconnect | history index and counter selection kept; settings selection reset | reconnect and read a value without selecting first |
| 22 | Combined bolus | decoded as `BolusKind::Combined` (types 17/18/31), distinct from a plain extended bolus; total in `v0`, immediate in `v1`, duration minutes in `v2`; sequences from 1 per log | run a combined bolus and read its status: confirm `v1` is the immediate part and the delivered field holds the total |
| 23 | Default values | 10-byte supervisor value and synthetic settings are placeholders | read them on the pump |
| 24 | BLE view of the 12-byte history payload | the entry's three `u16` are the payload's first 6 bytes (`v0` low, `v0` high, `v1` low): every layout in [history-firmware.md](history-firmware.md) fits them, so 33/34 read new then previous and 23 reads a `u32` | lower the bolus cap from 25 to 10 U on the pump, read the type 33 entry: expect `[1000, 2500, _]` |
| 25 | Daily total | written only on a SystemDate write to another day, stamped before the change; no midnight or storage-mode record; priming not counted | leave the pump over midnight, prime, then read the type 23 entries |
| 26 | Negative caps in 33/34 | read as Rockit's defaults for settings 112/113 (30.0 U, 40.0 U/h) | change a cap on a pump that never had one set, read the previous value |
| 27 | Running-record re-read floor | `history(after)` keeps a floor only for an in-progress bolus at or below the cursor (re-read in place); a cursor advanced past a still-running therapy event still misses its completion because the sans-IO `ReadHistory { after }` carries no separate `floor_sequence` like Rockit's sweep | confirm the app keeps its watermark below any running sequence, or add a floor to `Command::ReadHistory` |
| 28 | TBR actual-minutes re-read | `HistoryEvent::TempBasal` now carries a `phase` (type 9 = `Started`, 10/32 = `Ended`), but the re-read floor still tracks only in-progress boluses, so a started TBR rewritten in place (actual minutes on early cancel) is still not re-read by the floor | confirm the app re-reads a `Started` TBR before trusting its minutes, or add a floor that covers `Started` phases |
| 29 | Clock unwinding scope | clock-change records (types 12/13) unwind the UTC offset of older entries within the event log only; alarm and complaint entries older than a change keep the fixed offset, and the DRAFT reanchor/restart handling (Rockit `history.rs:461`, off by default) is not ported | confirm field logs where a clock change precedes alarm/complaint entries |
| 30 | Read-counter resync | every decrypted reply now adopts the pump's returned read counter and never fails on a stale one (Python reconciler); there is no persisted `read_resynced` flag (Rockit `engine.rs:1721`), so `store.rs` still merges the read counter forward (max) rather than trusting a saved reset | confirm the forward-merge is safe across app restarts after a key exchange |
| 31 | Event-decode phases (resolved in pump-shared `484032f`) | TBR phase is now distinct (type 9 = `Started`, 10/32 = `Ended`) and combined boluses decode as `BolusKind::Combined` (types 17/18/31); backup records 20/21/22 stay undecoded. Open: no distinct running-phase TBR record exists in firmware, so `Running` is never emitted, and aborted (29/30/31/32) still decode total-as-delivered rather than requested-vs-delivered | capture the aborted/combined records on the bench; confirm no firmware record maps to `TempBasalPhase::Running` |

## Therapy and clock command choices (driver)

What the therapy and clock paths decide that no single source fully settles.

| # | Point | Driver today | How to settle |
|---|---|---|---|
| T1 | TBR blackout gating | both the basal-blackout wait and the TBR-end (expiry) wait run unconditionally before a TBR start, like the bolus window. Rockit (`ypso-session/src/pump.rs:2179`) gates only the basal blackout on `avoid_window` (`ver_control < 1.3`) and runs the expiry wait always (`AVOID_CLEAR_TBR`). The extra basal wait on `control >= 1.3` pumps only ever delays a start by up to 14 s | confirm whether a `control >= 1.3` pump needs the basal blackout before a TBR, or restore the `avoid_window` gate on that one wait |
| T2 | Basal ceiling with no reported limit | settings 113 (cap) and 115 (max) are read; the effective ceiling is the smaller present one, a value `<= 0` is "no limit" (negative per COMMAND-SPEC §6; the simulator also reports an unset limit as 0). With neither present the ceiling is `i32::MAX / 10` hundredths (effectively unlimited) and the pump rejects at the write, as Rockit does rather than inventing 40 U/h. Setting 114 (min) `<= 0` is no lower bound | read 113/114/115 on a pump that never had them set and confirm negatives, and confirm a 0 cap/max should be "no limit" here rather than an error |
| T3 | Clock sync re-read | `SyncClock` writes with a 5 s midnight margin (time before date, `commands.rs`) and returns `Report::Done` without re-reading. Rockit re-reads to report drift-after; the sans-IO engine carries no drift in the outcome, so the re-read is omitted | confirm no caller needs a post-write clock read, else add it |
| T4 | Reconnect TBR status | after a reconnect `status()` reconstructs a running temp basal as `Basal::Temporary` from the pump's percent and the minutes-remaining field (`GetSystemStatus` offset 14), mirroring how a running extended bolus is reconstructed, instead of `Basal::Unknown` | confirm the minutes-remaining field is authoritative on hardware (bench item 4) |
| T5 | Pump mode access level | no firmware note covers the access level for a pump-mode (Suspend/Resume) write, so those commands stay `Unsupported` | probe a pump-mode write at each access level on hardware |
