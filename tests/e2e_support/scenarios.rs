use std::time::Duration;

use pump_shared::{
    Action, BasalProfile, BasalRate, BasalSegment, BleOp, Command, EngineState, Event, Failure,
    HistoryEvent, InsulinAmount, Outcome, Percent, PumpMode, Refusal, Report, Sequence, Slot,
    TempBasalRate, UtcTime,
};
use pump_testkit::Fault;
use pump_ypso::protocol::setting;

use super::*;

const HALF_HOUR: Duration = Duration::from_secs(1800);

pub fn units(milli: u32) -> InsulinAmount {
    InsulinAmount::from_milliunits(milli)
}

fn status_of(outcome: &Outcome) -> &pump_shared::PumpStatus {
    match outcome {
        Outcome::Completed(Report::Status(status)) => status,
        other => panic!("not a status: {other:?}"),
    }
}

fn stored_write<D>(run: &Run<D>) -> i64 {
    i64::from_le_bytes(run.harness.store[&Slot::Counters][..8].try_into().unwrap())
}

fn stored_epoch<D>(run: &Run<D>) -> u32 {
    u32::from_le_bytes(
        run.harness.store[&Slot::Counters][16..20]
            .try_into()
            .unwrap(),
    )
}

pub fn connects_and_reads_status<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert_eq!(run.engine.state(), EngineState::Paired);
    let outcome = run.run(Command::Status);
    let status = status_of(&outcome);
    assert_eq!(status.therapy.mode, PumpMode::Running);
    assert_eq!(status.battery, Some(Percent::new(80)));
    assert_eq!(status.reservoir, Some(units(150_000)));
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
    let again = run.run(Command::Status);
    assert_eq!(status_of(&again).therapy.mode, PumpMode::Running);
    assert!(!run
        .harness
        .transcript
        .iter()
        .any(|line| line.contains("Subscribe")));
}

pub fn reads_the_clock<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    match run.run(Command::ReadClock) {
        Outcome::Completed(Report::Clock(time)) => {
            assert!(time.local_seconds().abs_diff(NOON + 946_684_800) < 300)
        }
        other => panic!("{other:?}"),
    }
}

pub fn syncs_the_clock<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert_eq!(run.run(Command::SyncClock), Outcome::Failed(Failure::Host));
    let pump_now = |run: &Run<D>| (NOON + 946_684_800) * 1000 + run.harness.now.as_millis();
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

pub fn reads_and_writes_the_basal_profile<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
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
    assert_eq!(run.harness.device.sim().settings[&setting::basal_a(6)], 122);
    assert_eq!(run.harness.device.sim().settings[&setting::basal_a(23)], 0);
    match run.run(Command::ReadBasalProfile) {
        Outcome::Completed(Report::Profile(read)) => {
            assert_eq!(
                read.rate_at(Duration::from_secs(7 * 3600)),
                wanted.rate_at(Duration::from_secs(7 * 3600))
            )
        }
        other => panic!("{other:?}"),
    }
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
        .sim()
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

pub fn sets_and_cancels_a_temp_basal<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    let set = |percent| Command::TempBasal {
        rate: TempBasalRate::Percent(Percent::new(percent)),
        duration: HALF_HOUR,
    };
    if !cfg!(feature = "delivery") {
        assert_eq!(run.run(set(150)), Outcome::Refused(Refusal::NotBuilt));
        assert_eq!(
            run.run(Command::CancelTempBasal),
            Outcome::Completed(Report::Done)
        );
        return;
    }
    assert_eq!(run.run(set(150)), Outcome::Completed(Report::Done));
    assert_eq!(run.harness.device.sim().tbr_percent, 150);
    assert!(matches!(
        run.engine.therapy().basal,
        pump_shared::Basal::Temporary { .. }
    ));
    assert_eq!(
        status_of(&run.run(Command::Status)).therapy.basal,
        run.engine.therapy().basal
    );
    assert_eq!(run.run(set(80)), Outcome::Completed(Report::Done));
    assert_eq!(run.harness.device.sim().tbr_percent, 80);
    assert_eq!(run.run(set(100)), Outcome::Completed(Report::Done));
    assert_eq!(run.harness.device.sim().tbr_percent, 100);
    assert_eq!(run.run(set(80)), Outcome::Completed(Report::Done));
    assert_eq!(
        run.run(Command::CancelTempBasal),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.harness.device.sim().tbr_percent, 100);
    for refused in [
        Command::TempBasal {
            rate: TempBasalRate::Percent(Percent::new(260)),
            duration: HALF_HOUR,
        },
        Command::TempBasal {
            rate: TempBasalRate::Percent(Percent::new(220)),
            duration: Duration::from_secs(150 * 60),
        },
        Command::TempBasal {
            rate: TempBasalRate::Percent(Percent::new(150)),
            duration: Duration::from_secs(20 * 60),
        },
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

pub fn refuses_what_the_pump_cannot_do<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
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

pub fn unpairs<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    run.run(Command::Status);
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

pub fn survives_a_fault_at_every_step<D: Device>(make: impl Fn() -> D, fault: Fault) {
    let mut step = 1;
    loop {
        let mut run = Run::new(make(), &Blob::default());
        let before = run.harness.transcript.len();
        run.harness.faults.insert(before + step, fault);
        let id = run.submit(Command::Status);
        let outcome = run.outcome(id).expect("finished");
        if run.harness.transcript.len() < before + step {
            assert!(matches!(outcome, Outcome::Completed(_)));
            break;
        }
        assert!(
            matches!(
                outcome,
                Outcome::Failed(_) | Outcome::Unknown | Outcome::Completed(_)
            ),
            "{outcome:?}"
        );
        let old = std::mem::replace(&mut run.harness, harness(make(), &Blob::default()));
        run.harness.device = old.device;
        run.harness.store = old.store;
        run.harness.now = old.now;
        let again = run.run(Command::Status);
        assert!(
            matches!(again, Outcome::Completed(_)),
            "step {step}: {again:?}"
        );
        step += 1;
    }
    assert!(step > 30);
}

pub fn a_wrong_key_is_key_rejected<D: Device>(mut device: D) {
    device.sim().key = [1; 32];
    let blob = Blob {
        credentials: vec![(b"X1".to_vec(), PASSWORD)],
        ..Blob::default()
    };
    let mut run = Run::new(device, &blob);
    for _ in 0..2 {
        assert_eq!(
            run.run(Command::Status),
            Outcome::Failed(Failure::KeyRejected)
        );
    }
    assert_eq!(run.engine.state(), EngineState::KeyRejected);
}

pub fn jumps_a_counter_moved_by_another_app<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
    run.harness.device.sim().app_counter += 5_000;
    assert!(matches!(
        run.run(Command::ReadBasalProfile),
        Outcome::Completed(_)
    ));
    assert!(stored_write(&run) > 5_000);
}

pub fn gives_up_after_the_largest_jump<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
    run.harness.device.sim().app_counter = u64::MAX - 1;
    assert_eq!(
        run.run(Command::ReadBasalProfile),
        Outcome::Failed(Failure::Status(0x8B))
    );
}

pub fn follows_a_pump_reboot<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
    run.harness.device.sim().reboot();
    let failed = run.run(Command::ReadBasalProfile);
    assert!(
        matches!(failed, Outcome::Failed(Failure::Disconnected)),
        "{failed:?}"
    );
    assert!(matches!(
        run.run(Command::ReadBasalProfile),
        Outcome::Completed(_)
    ));
    assert_eq!(stored_epoch(&run), 1);
}

pub fn learns_the_epoch_before_jumping<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
    run.harness.device.sim().reboot += 1;
    assert!(matches!(
        run.run(Command::ReadBasalProfile),
        Outcome::Completed(_)
    ));
    assert_eq!(stored_epoch(&run), 1);
}

pub fn restarts_from_stored_slots<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
    run.engine = engine();
    run.settle();
    assert_eq!(run.engine.state(), EngineState::Paired);
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
    assert_eq!(
        run.harness.device.sim().app_counter as i64,
        stored_write(&run)
    );
}

pub fn restores_old_storage<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
    let old = run.harness.store.clone();
    assert!(matches!(
        run.run(Command::ReadBasalProfile),
        Outcome::Completed(_)
    ));
    run.harness.store = old;
    run.engine = engine();
    run.settle();
    assert!(matches!(
        run.run(Command::ReadBasalProfile),
        Outcome::Completed(_)
    ));
}

pub fn ignores_notifications<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    run.engine.handle_notification(pump_shared::Notification {
        service: pump_ypso::protocol::uuids::CONTROL_SERVICE,
        characteristic: pump_ypso::protocol::uuids::CONTROL_NOTIFY,
        data: vec![1, 2, 3],
    });
    assert!(run.engine.poll_action().is_none());
    assert!(matches!(run.run(Command::Status), Outcome::Completed(_)));
}

pub fn drives_an_opn_pump_in_clear<D: Device>(device: D) {
    let blob = Blob {
        version: 4,
        flags: 1,
        ..Blob::default()
    };
    let mut run = Run::new(device, &blob);
    let outcome = run.run(Command::Status);
    assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
    assert!(!run.harness.store.contains_key(&Slot::Counters));
    assert!(matches!(
        run.run(Command::ReadBasalProfile),
        Outcome::Completed(_)
    ));
}

pub fn never_drives_an_encrypted_pump_in_clear<D: Device>(device: D) {
    let blob = Blob {
        version: 4,
        flags: 1,
        ..Blob::default()
    };
    let mut run = Run::new(device, &blob);
    assert!(matches!(run.run(Command::Status), Outcome::Failed(_)));
    assert!(!run
        .harness
        .transcript
        .iter()
        .any(|l| l.contains("SystemStatus")
            || l.contains(&format!(
                "{:?}",
                pump_ypso::protocol::uuids::START_STOP_BOLUS
            ))));
}

pub fn boluses<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
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

pub fn stop_preempts_a_bolus<D: Device>(device: D) {
    if !cfg!(feature = "delivery") {
        return;
    }
    let mut run = Run::new(device, &Blob::default());
    run.run(Command::Status);
    let bolus = run
        .engine
        .submit(Command::Bolus {
            amount: units(5_000),
        })
        .unwrap();
    run.harness.run(run.engine.as_mut()).unwrap();
    run.harness
        .advance(run.engine.as_mut(), Duration::from_secs(5))
        .unwrap();
    let stop = run.engine.stop();
    assert_eq!(run.engine.stop(), stop);
    run.settle();
    assert_eq!(run.outcome(bolus), Some(Outcome::Unknown));
    assert_eq!(run.outcome(stop), Some(Outcome::Completed(Report::Done)));
    match run.run(Command::ReadHistory {
        after: Sequence::new(0),
    }) {
        Outcome::Completed(Report::History(entries)) => assert!(matches!(
            entries[0].event,
            HistoryEvent::Bolus { delivered, in_progress: false, .. } if delivered < units(5_000)
        )),
        other => panic!("{other:?}"),
    }
}

pub fn a_timed_out_bolus_is_unknown_and_never_resent<D: Device>(device: D) {
    if !cfg!(feature = "delivery") {
        return;
    }
    let mut run = Run::new(device, &Blob::default());
    run.run(Command::Status);
    let id = run
        .engine
        .submit(Command::Bolus {
            amount: units(1_000),
        })
        .unwrap();
    let mut actions = run.harness.transcript.len();
    loop {
        let Some(action) = run.engine.poll_action() else {
            panic!("no write");
        };
        actions += 1;
        let is_bolus = matches!(&action, Action::Ble { op: BleOp::Write { characteristic, .. }, .. } if *characteristic == pump_ypso::protocol::uuids::START_STOP_BOLUS);
        run.harness.faults.insert(actions, Fault::BleTimeout);
        if is_bolus {
            run.harness.transcript.push(format!("{action:?}"));
            run.engine
                .handle_completion(
                    action.id(),
                    pump_shared::Completion::Ble(pump_shared::BleResult::Timeout),
                )
                .unwrap();
            break;
        }
        run.harness.faults.clear();
        run.harness.transcript.push(format!("{action:?}"));
        let completion = match action {
            Action::Store { .. } => pump_shared::Completion::Stored,
            Action::Ble { ref op, .. } => {
                pump_shared::Completion::Ble(run.harness.device.handle(op, run.harness.now))
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

pub fn extended_bolus<D: Device>(device: D) {
    let mut run = Run::new(device, &Blob::default());
    let start = Command::ExtendedBolus {
        amount: units(2_000),
        duration: Duration::from_secs(3600),
    };
    if !cfg!(feature = "delivery") {
        assert_eq!(run.run(start), Outcome::Refused(Refusal::NotBuilt));
        return;
    }
    run.run(Command::Status);
    assert_eq!(run.run(start), Outcome::Completed(Report::Done));
    assert!(matches!(
        run.engine.therapy().extended,
        pump_shared::Extended::Running { .. }
    ));
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(run.engine.therapy().extended, pump_shared::Extended::Idle);
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    assert_eq!(
        run.run(Command::ExtendedBolus {
            amount: units(50),
            duration: Duration::from_secs(3600),
        }),
        Outcome::Refused(Refusal::OutOfLimits)
    );
}

pub fn reads_history<D: Device + Logs>(mut device: D) {
    let base = device.log(10, [150, 30, 0]);
    device.log(2, [250, 0, 0]);
    device.log(19, [300, 0, 0]);
    device.log(14, [3, 0, 0]);
    device.log(23, [1, 2, 3]);
    let mut run = Run::new(device, &Blob::default());
    match run.run(Command::ReadHistory {
        after: Sequence::new(0),
    }) {
        Outcome::Completed(Report::History(entries)) => {
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
        }
        other => panic!("{other:?}"),
    }
    match run.run(Command::ReadHistory {
        after: Sequence::new(base),
    }) {
        Outcome::Completed(Report::History(entries)) => assert_eq!(entries.len(), 3),
        other => panic!("{other:?}"),
    }
}

pub trait Logs {
    fn log(&mut self, kind: u8, values: [u16; 3]) -> u32;
}

impl Logs for Pump {
    fn log(&mut self, kind: u8, values: [u16; 3]) -> u32 {
        Pump::log(self, kind, values)
    }
}

impl Logs for YpsoSim {
    fn log(&mut self, _kind: u8, _values: [u16; 3]) -> u32 {
        0
    }
}

pub type Act<D> = fn(&mut Run<D>) -> pump_shared::CommandId;
pub type Prepare<D> = fn(&mut Run<D>);

pub fn fault_sweep<D: Device>(
    make: &dyn Fn() -> D,
    fault: Fault,
    prepare: Prepare<D>,
    act: Act<D>,
) {
    let mut step = 1;
    loop {
        let mut run = Run::new(make(), &Blob::default());
        prepare(&mut run);
        let before = run.harness.transcript.len();
        run.harness.faults.insert(before + step, fault);
        let id = act(&mut run);
        run.settle();
        let outcome = run.outcome(id).expect("finished");
        if run.harness.transcript.len() < before + step {
            break;
        }
        let old = std::mem::replace(&mut run.harness, harness(make(), &Blob::default()));
        run.harness.device = old.device;
        run.harness.store = old.store;
        run.harness.now = old.now;
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

pub fn nothing<D: Device>(_: &mut Run<D>) {}

pub fn read_status<D: Device>(run: &mut Run<D>) {
    run.run(Command::Status);
}

pub fn status_and_history<D: Device>(run: &mut Run<D>) {
    run.run(Command::Status);
    run.run(Command::ReadHistory {
        after: Sequence::new(0),
    });
}

pub fn start_extended<D: Device>(run: &mut Run<D>) {
    run.run(Command::Status);
    run.run(Command::ExtendedBolus {
        amount: units(2_000),
        duration: Duration::from_secs(3600),
    });
}

pub fn submit<D: Device>(run: &mut Run<D>, command: Command) -> pump_shared::CommandId {
    run.engine.submit(command).unwrap()
}

fn set_wall<D: Device>(run: &mut Run<D>) {
    run.engine.handle_wall_clock(UtcTime::from_unix_millis(
        (NOON + 946_684_800 + 3600) * 1000,
    ));
}

fn tbr_running<D: Device>(run: &mut Run<D>) {
    run.harness.device.sim().tbr_percent = 120;
}

fn temp_basal<D: Device>(run: &mut Run<D>) -> pump_shared::CommandId {
    submit(
        run,
        Command::TempBasal {
            rate: TempBasalRate::Percent(Percent::new(150)),
            duration: HALF_HOUR,
        },
    )
}

pub fn acts<D: Device>() -> Vec<(Prepare<D>, Act<D>)> {
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
        (read_status, temp_basal),
        (tbr_running, temp_basal),
        (nothing, |r| submit(r, Command::CancelTempBasal)),
        (status_and_history, |r| {
            submit(r, Command::Bolus { amount: units(500) })
        }),
        (read_status, |r| {
            submit(
                r,
                Command::ExtendedBolus {
                    amount: units(2_000),
                    duration: Duration::from_secs(3600),
                },
            )
        }),
        (start_extended, |r| submit(r, Command::CancelExtendedBolus)),
        (start_extended, |r| r.engine.stop()),
        (nothing, |r| r.engine.stop()),
        (nothing, |r| submit(r, Command::Unpair)),
    ]
}

pub fn sweeps_every_command<D: Device + Logs>(make: impl Fn() -> D, fault: Fault) {
    let logged = move || {
        let mut device = make();
        device.log(2, [100, 0, 0]);
        device
    };
    for (prepare, act) in acts::<D>() {
        fault_sweep(&logged, fault, prepare, act);
    }
}
