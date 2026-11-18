#![cfg(feature = "sim")]
#![cfg_attr(
    not(all(feature = "delivery", feature = "opn")),
    allow(dead_code, unused_imports)
)]

mod e2e_support;

use std::time::Duration;

use e2e_support::*;
use pump_shared::{
    Action, Basal, BasalProfile, BasalRate, BasalSegment, BleOp, BleResult, Command, CommandId,
    Completion, EngineState, Event, Extended, Failure, HistoryEvent, Notification, Outcome,
    Percent, PumpMode, Refusal, Report, Sequence, Slot, TempBasalRate, UtcTime,
};
use pump_ypso::protocol::{setting, uuids};
use pump_ypso::sim::YpsoSim;
use tester_harness::{Fault, SimDevice};

const HALF_HOUR: Duration = Duration::from_secs(1800);
const HOUR: Duration = Duration::from_secs(3600);

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
        run.run(Command::TempBasal {
            rate: TempBasalRate::Absolute(BasalRate::from_milliunits_per_hour(1_000)),
            duration: HALF_HOUR,
        }),
        Outcome::Refused(Refusal::Unsupported)
    );
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
        Command::Pair {
            address: pump_shared::Address::new(MAC),
            server_key: None,
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
    pump.events.push(stamp, 23, [1, 2, 3]);
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
            duration: HALF_HOUR
        }
    );
    assert_eq!(entries[3].sequence, Sequence::new(base));
    assert_eq!(entries[3].raw.len(), 19);
    assert_eq!(
        entries[3].pump_time.unwrap().local_seconds(),
        NOON as u64 + Y2K
    );
    assert_eq!(run.history(base).len(), 3);
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
