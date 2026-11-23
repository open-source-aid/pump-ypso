#![cfg(feature = "sim")]
#![cfg_attr(
    not(all(feature = "delivery", feature = "opn")),
    allow(dead_code, unused_imports)
)]

mod e2e_support;

use std::time::Duration;

use e2e_support::*;
use pump_shared::{
    Action, Address, Alarm, AlarmKind, Approximation, Basal, BasalProfile, BasalRate, BasalSegment,
    BleOp, BleResult, BolusKind, Command, CommandId, Completion, Component, EngineState, Event,
    Extended, Failure, HistoryEvent, KeyRef, LimitChange, Notification, Outcome, Percent, PumpMode,
    PumpTime, Refusal, Report, Sequence, Slot, TempBasalPhase, TempBasalRate, UtcTime,
};
use pump_ypso::protocol::{setting, uuids};
use pump_ypso::sim::{Log, YpsoSim, BOLUS_AMOUNT_CAP};
use tester_harness::{Fault, SimDevice};

const HALF_HOUR: Duration = Duration::from_secs(1800);
const HOUR: Duration = Duration::from_secs(3600);
const ALARM: u32 = 1 << 30;
const COMPLAINT: u32 = 2 << 30;
const EVENT: u32 = 3 << 30;

fn stored_write(run: &Run<YpsoSim>) -> i64 {
    i64::from_le_bytes(run.harness.store[&Slot::Counters][..8].try_into().unwrap())
}

fn stored_epoch(run: &Run<YpsoSim>) -> u32 {
    u32::from_le_bytes(
        run.harness.store[&Slot::Counters][16..20]
            .try_into()
            .unwrap(),
    )
}

fn completed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Completed(_))
}

fn temp_basal(percent: u16, duration: Duration) -> Command {
    Command::TempBasal {
        rate: TempBasalRate::Percent(Percent::new(percent)),
        duration,
    }
}

fn extended() -> Command {
    Command::ExtendedBolus {
        amount: units(2_000),
        duration: HOUR,
    }
}

#[test]
fn connects_and_reads_status() {
    let mut run = Run::new(sim(), &Blob::default());
    assert_eq!(run.engine.state(), EngineState::Paired);
    let status = run.status();
    assert_eq!(status.therapy.mode, PumpMode::Running);
    assert_eq!(status.battery, Some(Percent::new(80)));
    let reservoir = run.harness.device.reservoir as u32 * 10;
    assert!(reservoir < 150_000);
    assert_eq!(status.reservoir, Some(units(reservoir)));
    let info = run
        .harness
        .events
        .iter()
        .find_map(|e| match e {
            Event::DeviceInfo(info) => Some(info.clone()),
            _ => None,
        })
        .expect("device info");
    assert_eq!(info.serial, SERIAL.to_string());
    assert_eq!(info.firmware, "V05.02.03");
    assert_eq!(run.engine.therapy().mode, PumpMode::Running);
    assert_eq!(run.status().therapy.mode, PumpMode::Running);
    assert!(!run
        .harness
        .transcript
        .iter()
        .any(|line| line.contains("Subscribe")));
}

#[test]
fn reads_the_clock() {
    let mut run = Run::new(sim(), &Blob::default());
    match run.run(Command::ReadClock) {
        Outcome::Completed(Report::Clock(time)) => {
            assert!(time.local_seconds().abs_diff(NOON as u64 + Y2K) < 300)
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn syncs_the_clock() {
    let mut run = Run::new(sim(), &Blob::default());
    assert_eq!(run.run(Command::SyncClock), Outcome::Failed(Failure::Host));
    let pump_now = |run: &Run<YpsoSim>| (NOON as u64 + Y2K) * 1000 + run.harness.now.as_millis();
    let now = pump_now(&run);
    run.engine
        .handle_wall_clock(UtcTime::from_unix_millis(now + 1_000));
    assert_eq!(
        run.run(Command::SyncClock),
        Outcome::Completed(Report::Done)
    );
    let now = pump_now(&run);
    run.engine
        .handle_wall_clock(UtcTime::from_unix_millis(now + 3_600_500));
    assert_eq!(
        run.run(Command::SyncClock),
        Outcome::Completed(Report::Done)
    );
    match run.run(Command::ReadClock) {
        Outcome::Completed(Report::Clock(time)) => {
            let wanted = (now + 3_600_000) / 1000;
            assert!(time.local_seconds().abs_diff(wanted) < 300, "{time:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn reads_and_writes_the_basal_profile() {
    let mut run = Run::new(sim(), &Blob::default());
    let profile = match run.run(Command::ReadBasalProfile) {
        Outcome::Completed(Report::Profile(profile)) => profile,
        other => panic!("{other:?}"),
    };
    assert_eq!(profile.segments().len(), 24);
    assert_eq!(
        profile.rate_at(Duration::ZERO),
        BasalRate::from_milliunits_per_hour(800)
    );
    let wanted = BasalProfile::new(vec![
        BasalSegment {
            start: Duration::ZERO,
            rate: BasalRate::from_milliunits_per_hour(800),
        },
        BasalSegment {
            start: Duration::from_secs(6 * 3600),
            rate: BasalRate::from_milliunits_per_hour(1_220),
        },
        BasalSegment {
            start: Duration::from_secs(22 * 3600),
            rate: BasalRate::ZERO,
        },
    ])
    .unwrap();
    let set = run.run(Command::SetBasalProfile {
        profile: wanted.clone(),
    });
    if !cfg!(feature = "delivery") {
        assert_eq!(set, Outcome::Refused(Refusal::NotBuilt));
        return;
    }
    assert_eq!(set, Outcome::Completed(Report::Done));
    assert_eq!(run.harness.device.settings[&setting::basal_a(6)], 122);
    assert_eq!(run.harness.device.settings[&setting::basal_a(23)], 0);
    match run.run(Command::ReadBasalProfile) {
        Outcome::Completed(Report::Profile(read)) => {
            assert_eq!(
                read.rate_at(Duration::from_secs(7 * 3600)),
                wanted.rate_at(Duration::from_secs(7 * 3600))
            )
        }
        other => panic!("{other:?}"),
    }
    let changed = run.history(0);
    assert_eq!(changed.len(), 18);
    assert!(changed
        .iter()
        .all(|entry| entry.event == HistoryEvent::BasalProfileChanged));
    let off_grid = BasalProfile::new(vec![BasalSegment {
        start: Duration::ZERO,
        rate: BasalRate::from_milliunits_per_hour(1_010),
    }])
    .unwrap();
    assert_eq!(
        run.run(Command::SetBasalProfile { profile: off_grid }),
        Outcome::Refused(Refusal::OutOfLimits)
    );
    let half_hour = BasalProfile::new(vec![
        BasalSegment {
            start: Duration::ZERO,
            rate: BasalRate::from_milliunits_per_hour(800),
        },
        BasalSegment {
            start: HALF_HOUR,
            rate: BasalRate::from_milliunits_per_hour(900),
        },
    ])
    .unwrap();
    assert_eq!(
        run.run(Command::SetBasalProfile { profile: half_hour }),
        Outcome::Refused(Refusal::OutOfLimits)
    );
    run.harness
        .device
        .settings
        .insert(setting::BASAL_PROFILE_SELECTION, 4);
    assert_eq!(
        run.run(Command::ReadBasalProfile),
        Outcome::Refused(Refusal::Unsupported)
    );
    assert_eq!(
        run.run(Command::SetBasalProfile { profile: wanted }),
        Outcome::Refused(Refusal::Unsupported)
    );
}

#[test]
fn sets_and_cancels_a_temp_basal() {
    let mut run = Run::new(sim(), &Blob::default());
    let set = |percent| temp_basal(percent, HALF_HOUR);
    if !cfg!(feature = "delivery") {
        assert_eq!(run.run(set(150)), Outcome::Refused(Refusal::NotBuilt));
        assert_eq!(
            run.run(Command::CancelTempBasal),
            Outcome::Completed(Report::Done)
        );
        return;
    }
    let running = |percent| Basal::Temporary {
        rate: TempBasalRate::Percent(Percent::new(percent)),
        duration: HALF_HOUR,
    };
    assert_eq!(run.run(set(150)), Outcome::Completed(Report::Done));
    assert_eq!(run.engine.therapy().basal, running(150));
    assert_eq!(run.status().therapy.basal, running(150));
    assert_eq!(run.run(set(80)), Outcome::Completed(Report::Done));
    assert_eq!(run.status().therapy.basal, running(80));
    assert_eq!(run.run(set(100)), Outcome::Completed(Report::Done));
    assert_eq!(run.status().therapy.basal, Basal::Scheduled);
    assert_eq!(run.run(set(80)), Outcome::Completed(Report::Done));
    assert_eq!(
        run.run(Command::CancelTempBasal),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.status().therapy.basal, Basal::Scheduled);
    for refused in [
        set(260),
        temp_basal(220, Duration::from_secs(150 * 60)),
        temp_basal(150, Duration::from_secs(20 * 60)),
    ] {
        assert_eq!(run.run(refused), Outcome::Refused(Refusal::OutOfLimits));
    }
    assert_eq!(
        run.run(absolute(1_200, HALF_HOUR)),
        Outcome::Completed(Report::Done)
    );
    let therapy = run.status().therapy;
    assert_eq!(therapy.basal, suspended(HALF_HOUR));
    assert_eq!(therapy.extended, running_extended(600, 30));
}

fn absolute(milli: u32, duration: Duration) -> Command {
    Command::TempBasal {
        rate: TempBasalRate::Absolute(BasalRate::from_milliunits_per_hour(milli)),
        duration,
    }
}

fn running_extended(milli: u32, window_minutes: u64) -> Extended {
    Extended::Running {
        amount: units(milli),
        duration: Duration::from_secs(window_minutes * 60),
    }
}

fn suspended(duration: Duration) -> Basal {
    Basal::Temporary {
        rate: TempBasalRate::Percent(Percent::new(0)),
        duration,
    }
}

#[cfg(feature = "delivery")]
#[test]
fn delivers_an_absolute_temp_basal_as_an_extended_bolus() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    let mut deliver = |rate, duration, milli, window| {
        assert_eq!(
            run.run(absolute(rate, duration)),
            Outcome::Completed(Report::Done)
        );
        let therapy = run.status().therapy;
        assert_eq!(therapy.basal, suspended(duration));
        assert_eq!(therapy.extended, running_extended(milli, window));
        assert_eq!(
            run.run(Command::CancelExtendedBolus),
            Outcome::Completed(Report::Done)
        );
        assert_eq!(
            run.run(Command::CancelTempBasal),
            Outcome::Completed(Report::Done)
        );
    };
    // The requested rate becomes a bolus of rate * window / 60, delivered over a
    // window stretched or shrunk to land the amount inside the bolus limits.
    deliver(1_200, HALF_HOUR, 600, 30);
    deliver(820, HALF_HOUR, 400, 30);
    deliver(1_200, Duration::from_secs(20 * 60), 400, 20);
    deliver(3_000, HALF_HOUR, 1_500, 30);
    deliver(60_000, HOUR, 15_000, 15);
    deliver(101_000, Duration::from_secs(15 * 60), 25_000, 15);

    assert_eq!(
        run.run(absolute(0, HALF_HOUR)),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.status().therapy.basal, suspended(HALF_HOUR));
    assert_eq!(run.status().therapy.extended, Extended::Idle);
    assert_eq!(
        run.run(Command::CancelTempBasal),
        Outcome::Completed(Report::Done)
    );

    for refused in [
        absolute(200_000, HALF_HOUR),
        absolute(60_000, Duration::from_secs(1_000_000_000_000)),
    ] {
        assert_eq!(run.run(refused), Outcome::Refused(Refusal::OutOfLimits));
    }
}

#[cfg(feature = "delivery")]
#[test]
fn an_absolute_temp_basal_stretches_a_low_rate_over_a_longer_window() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    // A rate too low to make the minimum bolus over the asked duration stretches
    // the window until the amount clears the floor, independent of the schedule.
    assert_eq!(
        run.run(absolute(100, HALF_HOUR)),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.status().therapy.extended, running_extended(100, 60));
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(
        run.run(Command::CancelTempBasal),
        Outcome::Completed(Report::Done)
    );
    // A rate that stays below the floor even stretched to the limit is refused.
    assert_eq!(
        run.run(absolute(50, HALF_HOUR)),
        Outcome::Refused(Refusal::OutOfLimits)
    );
    // The extended path needs no readable schedule, so an unsupported profile
    // selection does not stop it.
    run.harness
        .device
        .settings
        .insert(setting::BASAL_PROFILE_SELECTION, 4);
    assert_eq!(
        run.run(absolute(1_000, HALF_HOUR)),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.status().therapy.extended, running_extended(500, 30));
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(
        run.run(Command::CancelTempBasal),
        Outcome::Completed(Report::Done)
    );
}

#[cfg(all(feature = "delivery", feature = "opn"))]
#[test]
fn an_absolute_temp_basal_needs_the_extended_bolus() {
    let mut pump = opn();
    pump.versions.pump_base = "1.0".into();
    let mut run = Run::new(pump, &Blob::opn());
    run.status();
    for rate in [1_200, 3_000] {
        assert_eq!(
            run.run(absolute(rate, HALF_HOUR)),
            Outcome::Refused(Refusal::Unsupported)
        );
    }
}

#[test]
fn refuses_what_the_pump_cannot_do() {
    let mut run = Run::new(sim(), &Blob::default());
    for command in [
        Command::Suspend,
        Command::Resume,
        Command::SetDeliveryLimits {
            max_bolus: units(10_000),
            max_basal: BasalRate::from_milliunits_per_hour(2_000),
        },
    ] {
        assert_eq!(run.run(command), Outcome::Refused(Refusal::Unsupported));
    }
    assert!(!run.harness.transcript.iter().any(|l| l.contains("Ble")));
}

#[test]
fn unpairs() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    assert_eq!(run.run(Command::Unpair), Outcome::Completed(Report::Done));
    assert_eq!(run.engine.state(), EngineState::Unpaired);
    assert!(run.harness.store.is_empty());
    assert_eq!(
        run.run(Command::Status),
        Outcome::Refused(Refusal::NotPaired)
    );
    let stop = run.engine.stop();
    run.settle();
    assert_eq!(
        run.outcome(stop),
        Some(Outcome::Refused(Refusal::NotPaired))
    );
}

fn survives_a_fault_at_every_step(fault: Fault) {
    let mut step = 1;
    loop {
        let mut run = Run::new(sim(), &Blob::default());
        let before = run.harness.transcript.len();
        run.harness.faults.insert(before + step, fault);
        let id = run.submit(Command::Status);
        let outcome = run.outcome(id).expect("finished");
        if run.harness.transcript.len() < before + step {
            assert!(completed(outcome));
            break;
        }
        assert!(
            matches!(
                outcome,
                Outcome::Failed(_) | Outcome::Unknown | Outcome::Completed(_)
            ),
            "{outcome:?}"
        );
        let mut run = run;
        let again = run.run(Command::Status);
        assert!(completed(again.clone()), "step {step}: {again:?}");
        step += 1;
    }
    assert!(step > 30);
}

#[test]
fn survives_a_timeout_at_every_step() {
    survives_a_fault_at_every_step(Fault::BleTimeout);
}

#[test]
fn survives_a_disconnect_at_every_step() {
    survives_a_fault_at_every_step(Fault::BleDisconnected);
}

#[test]
fn survives_a_host_failure_at_every_step() {
    survives_a_fault_at_every_step(Fault::HostFails);
}

#[test]
fn survives_a_counter_error_at_every_step() {
    survives_a_fault_at_every_step(Fault::BleStatus(0x8B));
}

#[test]
fn a_wrong_key_is_key_rejected() {
    let mut pump = sim();
    pump.key = Some([1; 32]);
    let blob = Blob {
        credentials: vec![(b"X1".to_vec(), PASSWORD)],
        ..Blob::default()
    };
    let mut run = Run::new(pump, &blob);
    for _ in 0..2 {
        assert_eq!(
            run.run(Command::Status),
            Outcome::Failed(Failure::KeyRejected)
        );
    }
    assert_eq!(run.engine.state(), EngineState::KeyRejected);
}

#[test]
fn jumps_a_counter_moved_by_another_app() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.device.app_counter += 5_000;
    assert!(completed(run.run(Command::ReadBasalProfile)));
    assert!(stored_write(&run) > 5_000);
}

#[test]
fn gives_up_after_the_largest_jump() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.device.app_counter = i64::MAX as u64 - 1;
    assert_eq!(
        run.run(Command::ReadBasalProfile),
        Outcome::Failed(Failure::Status(0x8B))
    );
}

#[test]
fn follows_a_pump_reboot() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    let now = run.harness.now;
    run.harness.device.reboot(now);
    let failed = run.run(Command::ReadBasalProfile);
    assert_eq!(failed, Outcome::Failed(Failure::Disconnected));
    assert!(completed(run.run(Command::ReadBasalProfile)));
    assert_eq!(stored_epoch(&run), 1);
}

#[test]
fn learns_the_epoch_before_jumping() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.device.reboot += 1;
    assert!(completed(run.run(Command::ReadBasalProfile)));
    assert_eq!(stored_epoch(&run), 1);
}

#[test]
fn restarts_from_stored_slots() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.restart();
    assert_eq!(run.engine.state(), EngineState::Paired);
    run.status();
    assert_eq!(run.harness.device.app_counter as i64, stored_write(&run));
}

#[test]
fn restores_old_storage() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    let old = run.harness.store.clone();
    assert!(completed(run.run(Command::ReadBasalProfile)));
    run.harness.store = old;
    run.restart();
    assert!(completed(run.run(Command::ReadBasalProfile)));
}

#[test]
fn ignores_notifications() {
    let mut run = Run::new(sim(), &Blob::default());
    run.engine.handle_notification(Notification {
        service: uuids::CONTROL_SERVICE,
        characteristic: uuids::CONTROL_NOTIFY,
        data: vec![1, 2, 3],
    });
    assert!(run.engine.poll_action().is_none());
    run.status();
}

#[cfg(feature = "opn")]
#[test]
fn drives_an_opn_pump_in_clear() {
    let mut run = Run::new(opn(), &Blob::opn());
    run.status();
    assert!(!run.harness.store.contains_key(&Slot::Counters));
    assert!(completed(run.run(Command::ReadBasalProfile)));
}

#[cfg(feature = "opn")]
#[test]
fn never_drives_an_encrypted_pump_in_clear() {
    let mut run = Run::new(sim(), &Blob::opn());
    assert!(matches!(run.run(Command::Status), Outcome::Failed(_)));
    let bolus = format!("{:?}", uuids::START_STOP_BOLUS);
    assert!(!run
        .harness
        .transcript
        .iter()
        .any(|l| l.contains("SystemStatus") || l.contains(&bolus)));
}

#[test]
fn boluses() {
    let mut run = Run::new(sim(), &Blob::default());
    let bolus = Command::Bolus {
        amount: units(1_500),
    };
    if !cfg!(feature = "delivery") {
        assert_eq!(run.run(bolus), Outcome::Refused(Refusal::NotBuilt));
        return;
    }
    assert_eq!(
        run.run(bolus),
        Outcome::Completed(Report::Delivered(units(1_500)))
    );
    let progress: Vec<_> = run
        .harness
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Progress { delivered, .. } => Some(*delivered),
            _ => None,
        })
        .collect();
    assert!(progress.len() > 2);
    assert_eq!(progress.last(), Some(&units(1_500)));
    assert_eq!(run.engine.therapy().bolus, pump_shared::Bolus::Idle);
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_520)
        }),
        Outcome::Refused(Refusal::OutOfLimits)
    );
}

#[cfg(feature = "delivery")]
#[test]
fn stop_preempts_a_bolus() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    let bolus = run
        .engine
        .submit(Command::Bolus {
            amount: units(5_000),
        })
        .unwrap();
    run.harness
        .advance(run.engine.as_mut(), Duration::from_secs(5))
        .unwrap();
    let stop = run.engine.stop();
    assert_eq!(run.engine.stop(), stop);
    run.settle();
    assert_eq!(run.outcome(bolus), Some(Outcome::Unknown));
    assert_eq!(run.outcome(stop), Some(Outcome::Completed(Report::Done)));
    assert!(matches!(
        run.history(0)[0].event,
        HistoryEvent::Bolus { delivered, in_progress: false, .. }
            if delivered > units(0) && delivered < units(5_000)
    ));
}

#[cfg(feature = "delivery")]
#[test]
fn a_timed_out_bolus_is_unknown_and_never_resent() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    let id = run
        .engine
        .submit(Command::Bolus {
            amount: units(1_000),
        })
        .unwrap();
    let mut actions = run.harness.transcript.len();
    loop {
        let action = run.engine.poll_action().expect("a bolus write");
        actions += 1;
        run.harness.transcript.push(format!("{action:?}"));
        let completion = match action {
            Action::Ble {
                op: BleOp::Write { characteristic, .. },
                ..
            } if characteristic == uuids::START_STOP_BOLUS => {
                run.harness.faults.insert(actions, Fault::BleTimeout);
                run.engine
                    .handle_completion(action.id(), Completion::Ble(BleResult::Timeout))
                    .unwrap();
                break;
            }
            Action::Store { .. } => Completion::Stored,
            Action::Ble { ref op, .. } => {
                Completion::Ble(run.harness.device.handle(op, run.harness.now))
            }
            other => panic!("{other:?}"),
        };
        run.engine
            .handle_completion(action.id(), completion)
            .unwrap();
    }
    run.settle();
    assert_eq!(run.outcome(id), Some(Outcome::Unknown));
}

#[test]
fn extended_bolus() {
    let mut run = Run::new(sim(), &Blob::default());
    if !cfg!(feature = "delivery") {
        assert_eq!(run.run(extended()), Outcome::Refused(Refusal::NotBuilt));
        return;
    }
    run.status();
    assert_eq!(run.run(extended()), Outcome::Completed(Report::Done));
    assert_eq!(
        run.engine.therapy().extended,
        Extended::Running {
            amount: units(2_000),
            duration: HOUR
        }
    );
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.engine.therapy().extended, Extended::Idle);
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(
        run.run(Command::ExtendedBolus {
            amount: units(50),
            duration: HOUR,
        }),
        Outcome::Refused(Refusal::OutOfLimits)
    );
}

#[test]
fn reads_history() {
    let mut pump = sim();
    let stamp = NOON as u32;
    let base = pump.events.push(stamp, 10, [150, 30, 0]);
    pump.events.push(stamp, 2, [250, 0, 0]);
    pump.events.push(stamp, 19, [300, 0, 0]);
    pump.events.push(stamp, 14, [3, 0, 0]);
    pump.events.push(stamp, 20, [1, 2, 3]);
    let mut run = Run::new(pump, &Blob::default());
    let entries = run.history(0);
    let events: Vec<_> = entries.iter().map(|e| e.event).collect();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0], HistoryEvent::Suspended);
    assert!(matches!(
        events[1],
        HistoryEvent::Bolus {
            in_progress: true,
            ..
        }
    ));
    assert!(
        matches!(events[2], HistoryEvent::Bolus { delivered, .. } if delivered == units(2_500))
    );
    assert_eq!(
        events[3],
        HistoryEvent::TempBasal {
            rate: TempBasalRate::Percent(Percent::new(150)),
            duration: HALF_HOUR,
            phase: TempBasalPhase::Ended
        }
    );
    assert_eq!(entries[3].sequence, Sequence::new(EVENT | base));
    assert_eq!(entries[3].raw.len(), 19);
    assert_eq!(
        entries[3].pump_time.unwrap().local_seconds(),
        NOON as u64 + Y2K
    );
    assert_eq!(run.history(EVENT | base).len(), 3);
    assert_eq!(run.history(base).len(), 4);
}

#[test]
fn a_running_bolus_at_the_cursor_is_reread_but_a_settled_one_is_not() {
    let mut pump = sim();
    let running = pump.events.push(NOON as u32, 19, [300, 0, 0]);
    let mut run = Run::new(pump, &Blob::default());
    let entries = run.history(EVENT | running);
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        entries[0].event,
        HistoryEvent::Bolus {
            in_progress: true,
            ..
        }
    ));
    let mut pump = sim();
    let settled = pump.events.push(NOON as u32, 2, [250, 0, 0]);
    let mut run = Run::new(pump, &Blob::default());
    assert!(run.history(EVENT | settled).is_empty());
}

#[test]
fn clock_changes_unwind_the_offset_of_older_entries() {
    let mut pump = sim();
    pump.events.push(NOON as u32, 2, [100, 0, 0]);
    pump.events.push(NOON as u32, 13, [0, 0, 0]);
    pump.events.push(NOON as u32, 2, [200, 0, 0]);
    let mut run = Run::new(pump, &Blob::default());
    let entries = run.history(0);
    let at = |seq| {
        entries
            .iter()
            .find(|e| e.sequence.get() == EVENT | seq)
            .unwrap()
            .time
            .as_unix_millis() as i64
    };
    let step = -(NOON % 86_400);
    assert_eq!(at(1) - at(3), step * 1000);
}

fn bolus(
    kind: BolusKind,
    requested: u32,
    delivered: u32,
    minutes: u64,
    in_progress: bool,
) -> HistoryEvent {
    HistoryEvent::Bolus {
        kind,
        requested: units(requested),
        delivered: units(delivered),
        duration: Duration::from_secs(minutes * 60),
        automatic: false,
        in_progress,
    }
}

fn percent(percent: u16, minutes: u64, phase: TempBasalPhase) -> HistoryEvent {
    HistoryEvent::TempBasal {
        rate: TempBasalRate::Percent(Percent::new(percent)),
        duration: Duration::from_secs(minutes * 60),
        phase,
    }
}

fn fill(
    log: &mut Log,
    tag: u32,
    entries: Vec<(u8, [u16; 3], Option<HistoryEvent>)>,
) -> Vec<(u32, HistoryEvent)> {
    let mut expected: Vec<_> = entries
        .into_iter()
        .filter_map(|(kind, values, event)| {
            let sequence = log.push(NOON as u32, kind, values);
            event.map(|event| (tag | sequence, event))
        })
        .collect();
    expected.reverse();
    expected
}

fn step(milli: u32) -> HistoryEvent {
    HistoryEvent::LimitChanged(LimitChange::BolusStep {
        value: units(milli),
    })
}

fn daily(milli: u32) -> HistoryEvent {
    HistoryEvent::DailyTotal {
        day: PumpTime::from_local_seconds(NOON as u64 + Y2K),
        amount: units(milli),
    }
}

fn max_bolus(value: u32, previous: u32) -> HistoryEvent {
    HistoryEvent::LimitChanged(LimitChange::MaxBolus {
        value: units(value),
        previous: units(previous),
    })
}

fn max_basal(value: u32, previous: u32) -> HistoryEvent {
    HistoryEvent::LimitChanged(LimitChange::MaxBasal {
        value: BasalRate::from_milliunits_per_hour(value),
        previous: BasalRate::from_milliunits_per_hour(previous),
    })
}

#[test]
fn decodes_every_entry_of_every_log() {
    use BolusKind::{Combined, Extended, Normal};
    let mut pump = sim();
    let changed = HistoryEvent::ClockChanged {
        previous: PumpTime::from_local_seconds(NOON as u64 + Y2K),
    };
    let alarm = |kind, code| HistoryEvent::Alarm(Alarm { kind, code });
    let mut expected = fill(
        &mut pump.events,
        EVENT,
        vec![
            (
                3,
                [200, 60, 0],
                Some(bolus(Extended, 2_000, 2_000, 60, false)),
            ),
            (1, [200, 60, 0], Some(bolus(Extended, 2_000, 0, 60, true))),
            (
                30,
                [150, 40, 0],
                Some(bolus(Extended, 1_500, 1_500, 40, false)),
            ),
            (20, [150, 40, 0], None),
            (
                18,
                [300, 100, 90],
                Some(bolus(Combined, 3_000, 3_000, 90, false)),
            ),
            (
                17,
                [300, 100, 90],
                Some(bolus(Combined, 3_000, 0, 90, true)),
            ),
            (
                31,
                [250, 100, 45],
                Some(bolus(Combined, 2_500, 2_500, 45, false)),
            ),
            (21, [250, 100, 45], None),
            (2, [250, 0, 0], Some(bolus(Normal, 2_500, 2_500, 0, false))),
            (19, [300, 0, 0], Some(bolus(Normal, 3_000, 0, 0, true))),
            (29, [139, 0, 0], Some(bolus(Normal, 1_390, 1_390, 0, false))),
            (26, [120, 0, 0], Some(bolus(Normal, 1_200, 1_200, 0, false))),
            (27, [130, 0, 0], Some(bolus(Normal, 1_300, 0, 0, true))),
            (28, [40, 0, 0], Some(bolus(Normal, 400, 400, 0, false))),
            (
                9,
                [150, 30, 0],
                Some(percent(150, 30, TempBasalPhase::Started)),
            ),
            (
                10,
                [80, 30, 0],
                Some(percent(80, 30, TempBasalPhase::Ended)),
            ),
            (32, [0, 12, 0], Some(percent(0, 12, TempBasalPhase::Ended))),
            (22, [0, 12, 0], None),
            (12, [14, 3, 24], Some(changed)),
            (13, [23, 59, 58], Some(changed)),
            (6, [0; 3], Some(HistoryEvent::BasalProfileChanged)),
            (7, [3, 50, 40], Some(HistoryEvent::BasalProfileChanged)),
            (8, [3, 50, 40], Some(HistoryEvent::BasalProfileChanged)),
            (14, [10, 0, 0], Some(HistoryEvent::Resumed)),
            (14, [3, 0, 0], Some(HistoryEvent::Suspended)),
            (16, [0; 3], Some(HistoryEvent::Rewound)),
            (24, [0; 3], Some(HistoryEvent::Replaced(Component::Battery))),
            (
                25,
                [50, 0, 0],
                Some(HistoryEvent::Replaced(Component::Cannula)),
            ),
            (
                4,
                [1_500, 7, 0],
                Some(bolus(BolusKind::Priming, 15_000, 15_000, 0, false)),
            ),
            (5, [10, 0, 0], Some(step(100))),
            (5, [7, 0, 0], None),
            (5, [105, 0, 0], None),
            (5, [0, 0, 0], None),
            (23, [2_400, 0, 0], Some(daily(24_000))),
            (23, [0x86A0, 1, 0], Some(daily(1_000_000))),
            (33, [2_500, 3_000, 0], Some(max_bolus(25_000, 30_000))),
            (33, [0xFFFF, 1_000, 0], Some(max_bolus(30_000, 10_000))),
            (34, [300, 4_000, 0], Some(max_basal(3_000, 40_000))),
            (34, [500, 0x8000, 0], Some(max_basal(5_000, 40_000))),
        ],
    );
    expected.extend(fill(
        &mut pump.alarms,
        ALARM,
        vec![
            (100, [0; 3], Some(alarm(AlarmKind::Other, 100))),
            (101, [0; 3], Some(alarm(AlarmKind::BatteryEmpty, 101))),
            (102, [7, 0, 0], Some(alarm(AlarmKind::Other, 102))),
            (103, [0; 3], Some(alarm(AlarmKind::Other, 103))),
            (104, [0; 3], Some(alarm(AlarmKind::ReservoirEmpty, 104))),
            (105, [0; 3], Some(alarm(AlarmKind::Occlusion, 105))),
            (106, [0; 3], Some(alarm(AlarmKind::AutoOff, 106))),
            (107, [0; 3], Some(alarm(AlarmKind::Other, 107))),
            (108, [0; 3], Some(alarm(AlarmKind::Other, 108))),
            (99, [0; 3], None),
        ],
    ));
    expected.extend(fill(
        &mut pump.complaints,
        COMPLAINT,
        vec![
            (206, [17, 1, 0], Some(alarm(AlarmKind::Other, 17))),
            (205, [17, 0, 0], None),
            (
                204,
                [17, 2, 0],
                Some(HistoryEvent::AlarmCleared(Alarm {
                    kind: AlarmKind::Other,
                    code: 17,
                })),
            ),
            (215, [0; 3], None),
        ],
    ));
    let mut run = Run::new(pump, &Blob::default());
    let entries = run.history(0);
    let read: Vec<_> = entries
        .iter()
        .map(|e| (e.sequence.get(), e.event))
        .collect();
    assert_eq!(read, expected);
    assert!(entries.iter().all(|e| e.raw.len() == 19
        && e.pump_time == Some(PumpTime::from_local_seconds(NOON as u64 + Y2K))));
}

#[test]
fn a_date_change_records_the_daily_total() {
    let mut run = Run::new(sim(), &Blob::default());
    run.harness.advance(run.engine.as_mut(), HOUR).unwrap();
    let now = (NOON as u64 + Y2K) * 1000 + run.harness.now.as_millis();
    run.engine
        .handle_wall_clock(UtcTime::from_unix_millis(now + 86_400_000));
    assert_eq!(
        run.run(Command::SyncClock),
        Outcome::Completed(Report::Done)
    );
    let entries = run.history(0);
    let device = &run.harness.device;
    let delivered = (15_000 - device.reservoir - device.delivered_today) as u32;
    let totals: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e.event, HistoryEvent::DailyTotal { .. }))
        .collect();
    assert_eq!(totals.len(), 1);
    assert!(delivered > 0);
    assert_eq!(
        totals[0].event,
        HistoryEvent::DailyTotal {
            day: totals[0].pump_time.unwrap(),
            amount: units(delivered * 10),
        }
    );
}

#[test]
fn pump_side_changes_reach_history() {
    let mut run = Run::new(sim(), &Blob::default());
    let now = run.harness.now;
    let device = &mut run.harness.device;
    device.prime(150, now);
    device.change_setting(setting::BOLUS_STEP_SELECTION, 20, now);
    device.change_setting(BOLUS_AMOUNT_CAP, 1_000, now);
    device.change_setting(setting::BASAL_RATE_CAP, 300, now);
    device.change_setting(setting::TIME_FORMAT, 1, now);
    let events: Vec<_> = run.history(0).into_iter().map(|e| e.event).collect();
    assert_eq!(
        events[..4],
        [
            max_basal(3_000, 40_000),
            max_bolus(10_000, 25_000),
            step(200),
            bolus(BolusKind::Priming, 1_500, 1_500, 0, false),
        ]
    );
}

#[test]
fn reads_only_what_is_newer_in_every_log() {
    let mut pump = sim();
    let stamp = NOON as u32;
    pump.events.push(stamp - 600, 16, [0; 3]);
    pump.alarms.push(stamp - 900, 105, [0; 3]);
    pump.complaints.push(stamp - 900, 206, [3, 1, 0]);
    let cursor = pump.events.push(stamp - 300, 24, [0; 3]);
    pump.alarms.push(stamp - 300, 101, [0; 3]);
    let mut run = Run::new(pump, &Blob::default());
    let all = run.history(0);
    assert_eq!(all.len(), 5);
    let newest = all.iter().map(|e| e.sequence.get()).max().unwrap();
    assert_eq!(newest, EVENT | cursor);
    let device = &mut run.harness.device;
    device.events.push(stamp, 25, [0; 3]);
    device.alarms.push(stamp, 104, [0; 3]);
    device.complaints.push(stamp, 204, [3, 2, 0]);
    let read: Vec<_> = run
        .history(newest)
        .iter()
        .map(|e| e.sequence.get())
        .collect();
    assert_eq!(read, [EVENT | 3, ALARM | 3, ALARM | 2, COMPLAINT | 2]);
    assert_eq!(run.history(ALARM | 3).len(), 8);
}

#[cfg(feature = "delivery")]
#[test]
fn history_follows_what_the_pump_did() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    assert_eq!(
        run.run(temp_basal(150, HALF_HOUR)),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(
        run.run(Command::CancelTempBasal),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.run(extended()), Outcome::Completed(Report::Done));
    run.harness
        .advance(run.engine.as_mut(), Duration::from_secs(600))
        .unwrap();
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(
        run.run(temp_basal(80, HALF_HOUR)),
        Outcome::Completed(Report::Done)
    );
    run.harness
        .advance(run.engine.as_mut(), Duration::from_secs(300))
        .unwrap();
    let now = run.harness.now;
    run.harness
        .device
        .alarm(pump_ypso::sim::Alarm::Occlusion, now);
    let events: Vec<_> = run.history(0).into_iter().map(|e| e.event).collect();
    assert!(
        matches!(
            events[..],
            [
                HistoryEvent::Suspended,
                HistoryEvent::TempBasal { rate: TempBasalRate::Percent(p), duration: d, phase: TempBasalPhase::Ended },
                HistoryEvent::Bolus { kind: BolusKind::Extended, delivered, in_progress: false, .. },
                HistoryEvent::TempBasal { rate: TempBasalRate::Percent(q), phase: TempBasalPhase::Ended, .. },
                HistoryEvent::Alarm(Alarm { kind: AlarmKind::Occlusion, code: 105 }),
            ] if p == Percent::new(80) && q == Percent::new(150)
                && d >= Duration::from_secs(300) && d < HALF_HOUR
                && delivered > units(0) && delivered < units(2_000)
        ),
        "{events:?}"
    );
}

#[cfg(feature = "delivery")]
#[test]
fn a_temp_basal_decodes_started_while_running_then_ended() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    assert_eq!(
        run.run(temp_basal(150, HALF_HOUR)),
        Outcome::Completed(Report::Done)
    );
    let running = run.history(0);
    let started = running
        .iter()
        .find_map(|entry| match entry.event {
            HistoryEvent::TempBasal { rate, phase, .. } => Some((rate, phase)),
            _ => None,
        })
        .expect("a temp basal entry");
    assert_eq!(
        started,
        (
            TempBasalRate::Percent(Percent::new(150)),
            TempBasalPhase::Started
        )
    );
    assert_eq!(
        run.run(Command::CancelTempBasal),
        Outcome::Completed(Report::Done)
    );
    let ended = run.history(0);
    let phase = ended.iter().find_map(|entry| match entry.event {
        HistoryEvent::TempBasal { phase, .. } => Some(phase),
        _ => None,
    });
    assert_eq!(phase, Some(TempBasalPhase::Ended));
}

#[test]
fn a_priming_record_decodes_its_amount_past_the_motion_and_force_fields() {
    let mut run = Run::new(sim(), &Blob::default());
    run.harness
        .device
        .events
        .push(NOON as u32, 4, [1_500, 0x2307, 0x0001]);
    let event = run.history(0).first().map(|entry| entry.event);
    assert!(
        matches!(
            event,
            Some(HistoryEvent::Bolus {
                kind: BolusKind::Priming,
                requested,
                delivered,
                in_progress: false,
                ..
            }) if requested == units(15_000) && delivered == units(15_000)
        ),
        "{event:?}"
    );
}

#[test]
fn a_combined_bolus_decodes_as_combined_not_extended() {
    let mut run = Run::new(sim(), &Blob::default());
    run.harness
        .device
        .events
        .push(NOON as u32, 18, [300, 100, 90]);
    let entries = run.history(0);
    let bolus = entries.first().map(|entry| entry.event);
    assert!(
        matches!(
            bolus,
            Some(HistoryEvent::Bolus {
                kind: BolusKind::Combined,
                requested,
                delivered,
                duration,
                in_progress: false,
                ..
            }) if requested == units(3_000)
                && delivered == units(3_000)
                && duration == Duration::from_secs(90 * 60)
        ),
        "{bolus:?}"
    );
}

type Act = fn(&mut Run<YpsoSim>) -> CommandId;
type Prepare = fn(&mut Run<YpsoSim>);

fn submit(run: &mut Run<YpsoSim>, command: Command) -> CommandId {
    run.engine.submit(command).unwrap()
}

fn nothing(_: &mut Run<YpsoSim>) {}

fn read_status(run: &mut Run<YpsoSim>) {
    run.status();
}

fn status_and_history(run: &mut Run<YpsoSim>) {
    run.status();
    run.history(0);
}

fn start_extended(run: &mut Run<YpsoSim>) {
    run.status();
    run.run(extended());
}

fn start_temp_basal(run: &mut Run<YpsoSim>) {
    run.run(temp_basal(120, HALF_HOUR));
}

fn set_wall(run: &mut Run<YpsoSim>) {
    run.engine
        .handle_wall_clock(UtcTime::from_unix_millis((NOON as u64 + Y2K + 3600) * 1000));
}

fn acts() -> Vec<(Prepare, Act)> {
    vec![
        (nothing, |r| submit(r, Command::Status)),
        (nothing, |r| submit(r, Command::ReadClock)),
        (set_wall, |r| submit(r, Command::SyncClock)),
        (nothing, |r| {
            submit(
                r,
                Command::ReadHistory {
                    after: Sequence::new(0),
                },
            )
        }),
        (nothing, |r| submit(r, Command::ReadBasalProfile)),
        (nothing, |r| {
            submit(
                r,
                Command::SetBasalProfile {
                    profile: BasalProfile::new(vec![BasalSegment {
                        start: Duration::ZERO,
                        rate: BasalRate::from_milliunits_per_hour(900),
                    }])
                    .unwrap(),
                },
            )
        }),
        (read_status, |r| submit(r, temp_basal(150, HALF_HOUR))),
        (read_status, |r| submit(r, absolute(1_200, HALF_HOUR))),
        (read_status, |r| submit(r, absolute(3_000, HALF_HOUR))),
        (start_temp_basal, |r| submit(r, temp_basal(150, HALF_HOUR))),
        (nothing, |r| submit(r, Command::CancelTempBasal)),
        (status_and_history, |r| {
            submit(r, Command::Bolus { amount: units(500) })
        }),
        (read_status, |r| submit(r, extended())),
        (start_extended, |r| submit(r, Command::CancelExtendedBolus)),
        (start_extended, |r| r.engine.stop()),
        (nothing, |r| r.engine.stop()),
        (nothing, |r| submit(r, Command::Unpair)),
    ]
}

fn sweeps_every_command(make: fn() -> YpsoSim, fault: Fault) {
    for (prepare, act) in acts() {
        let mut step = 1;
        loop {
            let mut pump = make();
            pump.events.push(NOON as u32, 2, [100, 0, 0]);
            let mut run = Run::new(pump, &Blob::default());
            prepare(&mut run);
            let before = run.harness.transcript.len();
            run.harness.faults.insert(before + step, fault);
            let id = act(&mut run);
            run.settle();
            let outcome = run.outcome(id).expect("finished");
            if run.harness.transcript.len() < before + step {
                break;
            }
            let mut run = run;
            let again = run.run(Command::Status);
            assert!(
                matches!(
                    again,
                    Outcome::Completed(_) | Outcome::Refused(Refusal::NotPaired)
                ),
                "step {step} ({outcome:?}): {again:?}"
            );
            step += 1;
        }
    }
}

#[test]
fn every_command_survives_timeouts() {
    sweeps_every_command(sim, Fault::BleTimeout);
}

#[test]
fn every_command_survives_disconnects() {
    sweeps_every_command(sim, Fault::BleDisconnected);
}

#[test]
fn every_command_survives_counter_errors() {
    sweeps_every_command(sim, Fault::BleStatus(0x8B));
}

#[test]
fn every_command_survives_refusals() {
    sweeps_every_command(sim, Fault::BleStatus(0x87));
}

#[test]
fn every_command_survives_host_failures() {
    sweeps_every_command(sim, Fault::HostFails);
}

#[test]
fn every_command_survives_timeouts_on_an_older_control_service() {
    sweeps_every_command(
        || {
            let mut pump = sim();
            pump.versions.control = "1.2".into();
            pump
        },
        Fault::BleTimeout,
    );
}

#[cfg(feature = "delivery")]
fn rate_run() -> Run<YpsoSim> {
    let mut device = sim();
    device.settings.insert(setting::BOLUS_STEP_SELECTION, 10);
    let mut run = Run::new(device, &Blob::default());
    run.status();
    run
}

#[cfg(feature = "delivery")]
fn immediate_count(sim: &YpsoSim) -> usize {
    sim.events.entries.iter().filter(|e| e.kind == 2).count()
}

#[cfg(feature = "delivery")]
fn delivered_hundredths(sim: &YpsoSim) -> u64 {
    sim.events
        .entries
        .iter()
        .filter(|e| e.kind == 2)
        .map(|e| u64::from(e.values[0]))
        .sum()
}

#[cfg(feature = "delivery")]
fn marker_offset(sim: &YpsoSim) -> Option<u64> {
    let seconds: Vec<u64> = sim
        .events
        .entries
        .iter()
        .rev()
        .filter(|e| e.kind == 13)
        .map(|e| u64::from(e.values[2]))
        .collect();
    match seconds.as_slice() {
        [newer, older, ..] => Some((newer + 60 - older) % 60),
        _ => None,
    }
}

#[cfg(feature = "delivery")]
fn rate_bolus(milli: u32, seconds: u64) -> Command {
    Command::RateBolus {
        amount: units(milli),
        duration: Duration::from_secs(seconds),
    }
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_spreads_a_clean_sequence_and_marks_it() {
    let mut run = rate_run();
    assert_eq!(
        run.run(rate_bolus(1_000, 180)),
        Outcome::Completed(Report::Delivered(units(1_000)))
    );
    let sim = &run.harness.device;
    assert_eq!(immediate_count(sim), 10);
    assert_eq!(delivered_hundredths(sim), 100);
    assert_eq!(marker_offset(sim), Some(100 % 59));
    assert_eq!(run.engine.therapy().bolus, pump_shared::Bolus::Idle);
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_coarsens_and_marks_a_multiple_of_the_modulus() {
    let mut run = rate_run();
    assert_eq!(
        run.run(rate_bolus(5_900, 200)),
        Outcome::Completed(Report::Delivered(units(5_900)))
    );
    let sim = &run.harness.device;
    assert_eq!(immediate_count(sim), 10);
    assert_eq!(delivered_hundredths(sim), 590);
    assert_eq!(590 % 59, 0);
    assert_eq!(marker_offset(sim), Some(0));
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_off_the_step_grid_is_approximated() {
    let mut run = rate_run();
    assert_eq!(
        run.run(rate_bolus(250, 180)),
        Outcome::Approximated(Approximation::Bolus {
            requested: units(250),
            delivered: units(200),
            duration: Duration::from_secs(180),
        })
    );
    let sim = &run.harness.device;
    assert_eq!(immediate_count(sim), 2);
    assert_eq!(delivered_hundredths(sim), 20);
    assert_eq!(marker_offset(sim), Some(20));
}

#[cfg(feature = "delivery")]
#[test]
fn a_single_step_rate_bolus_near_midnight_offsets_the_clock() {
    let mut device = sim();
    device.settings.insert(setting::BOLUS_STEP_SELECTION, 10);
    device.clock = 9497 * 86_400 + 23 * 3_600 + 59 * 60;
    let mut run = Run::new(device, &Blob::default());
    run.status();
    assert_eq!(
        run.run(rate_bolus(100, 5)),
        Outcome::Completed(Report::Delivered(units(100)))
    );
    let sim = &run.harness.device;
    assert_eq!(immediate_count(sim), 1);
    assert_eq!(marker_offset(sim), Some(10));
    assert!(!sim
        .events
        .entries
        .iter()
        .any(|e| e.kind == 12 || e.kind == 23));
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_too_fast_is_out_of_limits() {
    let mut run = rate_run();
    assert_eq!(
        run.run(rate_bolus(1_000, 5)),
        Outcome::Refused(Refusal::OutOfLimits)
    );
    assert_eq!(immediate_count(&run.harness.device), 0);
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_of_too_many_micro_boluses_is_out_of_limits() {
    let mut device = sim();
    device.settings.insert(setting::BOLUS_STEP_SELECTION, 5);
    device.settings.insert(BOLUS_AMOUNT_CAP, 2_500);
    let mut run = Run::new(device, &Blob::default());
    run.status();
    assert_eq!(
        run.run(rate_bolus(25_000, 100_000)),
        Outcome::Refused(Refusal::OutOfLimits)
    );
    assert_eq!(immediate_count(&run.harness.device), 0);
}

#[cfg(feature = "delivery")]
#[test]
fn an_out_of_range_rate_bolus_is_out_of_limits() {
    let mut run = rate_run();
    assert_eq!(
        run.run(rate_bolus(50, 180)),
        Outcome::Refused(Refusal::OutOfLimits)
    );
    assert_eq!(
        run.run(rate_bolus(30_000, 180)),
        Outcome::Refused(Refusal::OutOfLimits)
    );
    assert_eq!(immediate_count(&run.harness.device), 0);
}

#[cfg(feature = "delivery")]
#[test]
fn stop_preempts_a_rate_bolus_without_double_delivery() {
    let mut run = rate_run();
    let id = run.engine.submit(rate_bolus(500, 600)).unwrap();
    run.harness
        .advance(run.engine.as_mut(), Duration::from_secs(5))
        .unwrap();
    let stop = run.engine.stop();
    run.settle();
    assert_eq!(run.outcome(id), Some(Outcome::Unknown));
    assert_eq!(run.outcome(stop), Some(Outcome::Completed(Report::Done)));
    let sim = &run.harness.device;
    let delivered = delivered_hundredths(sim);
    assert!(delivered > 0 && delivered < 50, "{delivered}");
    assert_eq!(marker_offset(sim), None);
}

#[cfg(feature = "delivery")]
#[test]
fn a_slow_rate_bolus_waits_between_micro_boluses() {
    let mut run = rate_run();
    assert_eq!(
        run.run(rate_bolus(200, 2_000)),
        Outcome::Completed(Report::Delivered(units(200)))
    );
    let sim = &run.harness.device;
    assert_eq!(immediate_count(sim), 2);
    assert_eq!(marker_offset(sim), Some(20));
}

#[cfg(feature = "delivery")]
fn a_rate_bolus_survives_a_fault_at_every_step(fault: Fault) {
    let mut step = 1;
    loop {
        let mut run = rate_run();
        let before = run.harness.transcript.len();
        run.harness.faults.insert(before + step, fault);
        let outcome = run.run(rate_bolus(200, 180));
        if run.harness.transcript.len() < before + step {
            assert!(completed(outcome.clone()), "{outcome:?}");
            break;
        }
        assert!(
            matches!(
                outcome,
                Outcome::Failed(_)
                    | Outcome::Unknown
                    | Outcome::Completed(_)
                    | Outcome::Approximated(_)
            ),
            "step {step}: {outcome:?}"
        );
        let again = run.run(Command::Status);
        assert!(completed(again.clone()), "step {step}: {again:?}");
        step += 1;
    }
    assert!(step > 10);
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_that_cannot_reread_after_a_drop_is_unknown() {
    let mut run = rate_run();
    let id = run.engine.submit(rate_bolus(200, 180)).unwrap();
    let mut injected = 0;
    while let Some(action) = run.engine.poll_action() {
        run.harness.transcript.push(format!("{action:?}"));
        let completion = match &action {
            Action::Ble {
                op: BleOp::Write { characteristic, .. },
                ..
            } if *characteristic == uuids::START_STOP_BOLUS && injected == 0 => {
                injected = 1;
                Completion::Ble(BleResult::Disconnected)
            }
            Action::Ble { .. } if injected == 1 => {
                injected = 2;
                Completion::Ble(BleResult::Disconnected)
            }
            Action::Store { .. } => Completion::Stored,
            Action::Ble { op, .. } => {
                Completion::Ble(run.harness.device.handle(op, run.harness.now))
            }
            other => panic!("{other:?}"),
        };
        run.engine
            .handle_completion(action.id(), completion)
            .unwrap();
    }
    assert_eq!(injected, 2);
    run.settle();
    assert_eq!(run.outcome(id), Some(Outcome::Unknown));
    assert_eq!(immediate_count(&run.harness.device), 0);
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_survives_a_disconnect_at_every_step() {
    a_rate_bolus_survives_a_fault_at_every_step(Fault::BleDisconnected);
}

#[cfg(feature = "delivery")]
#[test]
fn a_rate_bolus_survives_a_rejected_write_at_every_step() {
    a_rate_bolus_survives_a_fault_at_every_step(Fault::BleStatus(0x87));
}

#[cfg(feature = "delivery")]
#[test]
fn a_bolus_on_an_empty_log_never_probes_a_missing_entry() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.transcript.clear();
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_500)
        }),
        Outcome::Completed(Report::Delivered(units(1_500)))
    );
    let value = format!("{:?}", uuids::EVENT_ENTRY_VALUE);
    let index = format!("{:?}", uuids::EVENT_ENTRY_INDEX);
    let count = format!("{:?}", uuids::EVENT_ENTRY_COUNT);
    let dose = format!("{:?}", uuids::START_STOP_BOLUS);
    let before_dose: Vec<&String> = run
        .harness
        .transcript
        .iter()
        .take_while(|line| !line.contains(&dose))
        .collect();
    let touched = |needle: &str| before_dose.iter().any(|line| line.contains(needle));
    assert!(touched(&count));
    assert!(!touched(&value));
    assert!(!touched(&index));
}

#[cfg(feature = "delivery")]
#[test]
fn a_bolus_tolerates_a_count_without_a_readable_entry() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.device.events.capacity = Some(1);
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_500)
        }),
        Outcome::Completed(Report::Delivered(units(1_500)))
    );
}

// ===== Offline pairing ==========================================================================

use chacha20::cipher::consts::U10;
use curve25519_dalek::montgomery::MontgomeryPoint;
use pump_ypso::protocol::KEY_LEN;

/// The server secret the `YpsoSim` trusts (its default `server_secret`). The host's `key_agree`
/// uses it for the `KeyRef` the pair names, so the engine never sees it in the clear.
const SERVER: [u8; KEY_LEN] = [0x11; KEY_LEN];

fn derive(secret: [u8; KEY_LEN], peer: [u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let shared = MontgomeryPoint(peer).mul_clamped(secret).to_bytes();
    chacha20::hchacha::<U10>(&shared.into(), &[0u8; 16].into()).into()
}

/// The session key persisted in `Slot::Session` (the host wrap is the identity in the harness, so
/// the stored bytes are the `"YB"` blob and its key field sits at bytes 14..46).
fn stored_session_key(run: &Run<YpsoSim>) -> [u8; KEY_LEN] {
    run.harness.store[&Slot::Session][14..14 + KEY_LEN]
        .try_into()
        .unwrap()
}

#[test]
fn an_offline_pair_rekeys_the_pump_and_persists_the_agreed_session() {
    let mut run = Run::new(sim(), &Blob::default());
    assert_eq!(run.engine.state(), EngineState::Paired);
    let pump_public = run.harness.device.pump_public();
    let session = derive(SERVER, pump_public);

    let outcome = run.run(Command::Pair {
        address: Address::new(MAC),
        server_key: Some(KeyRef::new("server")),
    });

    assert_eq!(outcome, Outcome::Completed(Report::Done));
    assert_eq!(run.engine.state(), EngineState::Paired);
    // The session key is derive(X25519(server, pump_pub)): the pump holds it, and so does the store.
    assert_eq!(run.harness.device.key, Some(session));
    assert_eq!(stored_session_key(&run), session);
    assert_eq!(run.harness.device.pairing_counter, 1);
    assert_eq!(run.harness.device.reboot, 1);
}

#[test]
fn a_sealed_command_works_under_the_freshly_paired_key() {
    let mut run = Run::new(sim(), &Blob::default());
    assert_eq!(
        run.run(Command::Pair {
            address: Address::new(MAC),
            server_key: Some(KeyRef::new("server")),
        }),
        Outcome::Completed(Report::Done)
    );
    // Past the pump's ~5-minute post-pair cooldown, a sealed read opens under the new key.
    run.harness
        .advance(run.engine.as_mut(), Duration::from_secs(300))
        .expect("no violation");
    assert_eq!(run.status().therapy.mode, PumpMode::Running);
}

#[test]
fn an_offline_pair_without_a_server_key_is_refused() {
    let mut run = Run::new(sim(), &Blob::default());
    assert_eq!(
        run.run(Command::Pair {
            address: Address::new(MAC),
            server_key: None,
        }),
        Outcome::Refused(Refusal::NotPaired)
    );
    // The pump keeps whatever key it held; nothing was written.
    assert_eq!(run.harness.device.key, Some(KEY));
}

#[cfg(feature = "opn")]
#[test]
fn an_offline_pair_on_an_opn_pump_is_cleanly_unsupported() {
    let mut run = Run::new(opn(), &Blob::opn());
    assert_eq!(
        run.run(Command::Pair {
            address: Address::new(MAC),
            server_key: Some(KeyRef::new("server")),
        }),
        Outcome::Refused(Refusal::Unsupported)
    );
    // Refused before connecting: no offer read against a pump with no CryptoService.
    assert!(!run.harness.transcript.iter().any(|l| l.contains("Ble")));
}
