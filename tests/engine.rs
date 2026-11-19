#![cfg(feature = "sim")]
#![cfg_attr(
    not(all(feature = "delivery", feature = "opn")),
    allow(dead_code, unused_imports)
)]

mod e2e_support;

use std::collections::VecDeque;
use std::time::Duration;

use e2e_support::*;
use pump_shared::{
    Action, Basal, BasalProfile, BasalRate, BasalSegment, BleOp, BleResult, Bolus, Command,
    Completion, CompletionError, Driver, EngineState, Event, Extended, Failure, OpId, Outcome,
    Percent, PumpMode, Refusal, Report, Secret, Sequence, Slot, TempBasalRate, Timestamp, Uuid,
};
use pump_ypso::protocol::{setting, uuids, NONCE_LEN};
use pump_ypso::sim::{crc16, YpsoSim};
use pump_ypso::{Crypto, Frames, DRIVER};
use tester_harness::{Fault, SimDevice};

const DOSES: [Uuid; 3] = [
    uuids::START_STOP_BOLUS,
    uuids::START_STOP_TBR,
    uuids::SETTING_VALUE,
];

#[derive(Debug)]
struct Pump {
    sim: YpsoSim,
    deaf: bool,
    answer: Option<(Uuid, Vec<u8>)>,
    raw: Option<(Uuid, Vec<u8>)>,
    refuse: Option<(Uuid, u8)>,
    grow: VecDeque<usize>,
    tx: VecDeque<Vec<u8>>,
}

impl Pump {
    fn new(sim: YpsoSim) -> Self {
        Self {
            sim,
            deaf: false,
            answer: None,
            raw: None,
            refuse: None,
            grow: VecDeque::new(),
            tx: VecDeque::new(),
        }
    }

    fn seal(&mut self, plaintext: &[u8]) -> BleResult {
        self.sim.pump_counter += 1;
        let sealed = Crypto::new(self.sim.key.unwrap())
            .seal(
                plaintext,
                self.sim.reboot,
                self.sim.pump_counter,
                &[3; NONCE_LEN],
            )
            .unwrap();
        self.tx = Frames::split(&sealed).unwrap().into();
        BleResult::Data(self.tx.pop_front().unwrap())
    }
}

impl SimDevice for Pump {
    fn handle(&mut self, op: &BleOp, now: Timestamp) -> BleResult {
        match op {
            BleOp::Read { characteristic, .. } => {
                if let Some((_, bytes)) = self.raw.as_ref().filter(|(c, _)| c == characteristic) {
                    return BleResult::Data(bytes.clone());
                }
                if let Some((_, plaintext)) =
                    self.answer.clone().filter(|(c, _)| c == characteristic)
                {
                    return self.seal(&plaintext);
                }
                if *characteristic == uuids::EXTENDED_READ {
                    if let Some(frame) = self.tx.pop_front() {
                        return BleResult::Data(frame);
                    }
                }
                if *characteristic == uuids::EVENT_ENTRY_VALUE {
                    for _ in 0..self.grow.pop_front().unwrap_or(0) {
                        self.sim.events.push(0, 16, [0; 3]);
                    }
                }
            }
            BleOp::Write { characteristic, .. } if self.deaf && DOSES.contains(characteristic) => {
                return BleResult::Done;
            }
            BleOp::Write { characteristic, .. } => {
                if let Some((_, code)) = self.refuse.filter(|(c, _)| c == characteristic) {
                    return BleResult::Status(code);
                }
            }
            _ => {}
        }
        self.sim.handle(op, now)
    }
}

fn pump() -> Pump {
    Pump::new(sim())
}

fn read_all() -> Command {
    Command::ReadHistory {
        after: Sequence::new(0),
    }
}

fn tbr(percent: u16) -> Command {
    Command::TempBasal {
        rate: TempBasalRate::Percent(Percent::new(percent)),
        duration: Duration::from_secs(1800),
    }
}

fn extended(milli: u32) -> Command {
    Command::ExtendedBolus {
        amount: units(milli),
        duration: Duration::from_secs(3600),
    }
}

fn absolute(milli: u32) -> Command {
    Command::TempBasal {
        rate: TempBasalRate::Absolute(BasalRate::from_milliunits_per_hour(milli)),
        duration: Duration::from_secs(1800),
    }
}

fn entry(kind: u8, index: u16) -> Vec<u8> {
    let mut bytes = 0u32.to_le_bytes().to_vec();
    bytes.push(kind);
    bytes.extend([0; 6]);
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(index.to_le_bytes());
    let crc = crc16(&bytes);
    bytes.extend(crc.to_le_bytes());
    bytes
}

#[cfg(feature = "delivery")]
#[test]
fn a_pump_that_cannot_deliver_refuses_the_dose() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.device.reservoir = 0;
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_000)
        }),
        Outcome::Failed(Failure::Status(0x87))
    );
    run.harness.device.reservoir = 10_000;
    let now = run.harness.now;
    run.harness.device.stop(now);
    assert_eq!(run.status().therapy.mode, PumpMode::Suspended);
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_000)
        }),
        Outcome::Refused(Refusal::NotRunning)
    );
}

#[cfg(feature = "delivery")]
#[test]
fn an_unconfirmed_dose_is_unknown() {
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.deaf = true;
    assert_eq!(run.run(tbr(150)), Outcome::Unknown);
    run.harness.device.deaf = false;
    assert_eq!(run.run(extended(2_000)), Outcome::Completed(Report::Done));
    run.harness.device.deaf = true;
    assert_eq!(run.run(Command::CancelExtendedBolus), Outcome::Unknown);
    run.harness.device.deaf = false;
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
    run.harness.device.deaf = true;
    assert_eq!(
        run.run(Command::Bolus { amount: units(500) }),
        Outcome::Unknown
    );
    assert_eq!(run.run(extended(2_000)), Outcome::Unknown);
    let profile = BasalProfile::new(vec![BasalSegment {
        start: Duration::ZERO,
        rate: BasalRate::from_milliunits_per_hour(900),
    }])
    .unwrap();
    assert_eq!(
        run.run(Command::SetBasalProfile { profile }),
        Outcome::Unknown
    );
}

fn log_of(count: usize) -> Pump {
    let mut pump = pump();
    for _ in 0..count {
        pump.sim.events.push(0, 16, [0; 3]);
    }
    pump
}

#[test]
fn skips_corrupt_entries_without_failing_the_history() {
    let mut run = Run::new(log_of(4), &Blob::default());
    let mut corrupt = entry(16, 0);
    corrupt[18] ^= 0xFF;
    run.harness.device.answer = Some((uuids::EVENT_ENTRY_VALUE, corrupt));
    assert_eq!(
        run.run(read_all()),
        Outcome::Completed(Report::History(vec![]))
    );
}

#[test]
fn restarts_once_when_the_log_grows_then_settles() {
    let mut run = Run::new(log_of(4), &Blob::default());
    run.harness.device.grow = [0, 1].into();
    let sequences: Vec<_> = run
        .history(0)
        .iter()
        .map(|e| e.sequence.get() & 0xFFFF)
        .collect();
    assert_eq!(sequences, [5, 4, 3, 2, 1]);
}

#[test]
fn a_log_that_keeps_moving_is_tolerated_not_failed() {
    let mut run = Run::new(log_of(4), &Blob::default());
    run.harness.device.answer = Some((uuids::EVENT_ENTRY_VALUE, entry(16, 7)));
    assert_eq!(run.history(0).len(), 1);
    let mut run = Run::new(log_of(4), &Blob::default());
    run.harness.device.grow = [0, 1, 1, 1, 1, 1].into();
    assert!(matches!(
        run.run(read_all()),
        Outcome::Completed(Report::History(_))
    ));
}

#[test]
fn a_value_0x84_past_the_fill_ends_the_log_without_failing() {
    let mut pump = log_of(2);
    pump.sim.events.capacity = Some(12);
    let mut run = Run::new(pump, &Blob::default());
    assert_eq!(run.history(0).len(), 2);
}

#[test]
fn stops_at_the_end_of_the_log_when_the_count_is_its_capacity() {
    let mut pump = pump();
    pump.sim.events.push(0, 16, [0; 3]);
    let capacity = [10i32.to_le_bytes(), (!10i32).to_le_bytes()].concat();
    pump.answer = Some((uuids::EVENT_ENTRY_COUNT, capacity));
    let mut run = Run::new(pump, &Blob::default());
    assert_eq!(run.history(0).len(), 1);
}

#[test]
fn an_older_history_service_selects_entries_by_a_bare_index() {
    let mut pump = sim();
    pump.versions.history = "1.1".into();
    pump.events.push(0, 16, [0; 3]);
    pump.events.push(0, 16, [0; 3]);
    let mut run = Run::new(pump, &Blob::default());
    for _ in 0..2 {
        assert_eq!(run.history(0).len(), 2);
    }
}

#[test]
fn waits_out_the_basal_window_on_older_control_services() {
    let mut pump = sim();
    pump.versions.control = "1.2".into();
    pump.clock = NOON - 60 + 175;
    let mut run = Run::new(pump, &Blob::default());
    run.status();
    let start = run.harness.now;
    if cfg!(feature = "delivery") {
        assert_eq!(run.run(tbr(150)), Outcome::Completed(Report::Done));
        assert!(run.harness.now.as_millis() - start.as_millis() >= 5_000);
    }
    run.engine
        .handle_wall_clock(pump_shared::UtcTime::from_unix_millis(
            (NOON as u64 + Y2K + 3600) * 1000,
        ));
    assert_eq!(
        run.run(Command::SyncClock),
        Outcome::Completed(Report::Done)
    );
}

#[test]
fn rereads_the_clock_just_after_midnight() {
    let mut pump = sim();
    pump.clock = 9497 * 86_400 - 115;
    let mut run = Run::new(pump, &Blob::default());
    assert!(matches!(
        run.run(Command::ReadClock),
        Outcome::Completed(Report::Clock(_))
    ));
}

#[test]
fn refuses_malformed_replies() {
    for (characteristic, bytes, command) in [
        (uuids::SYSTEM_DATE, vec![0xFF; 4], Command::ReadClock),
        (uuids::GET_SYSTEM_STATUS, vec![1], Command::Status),
        (uuids::GET_BOLUS_STATUS, vec![0; 44], Command::Status),
    ] {
        let mut run = Run::new(pump(), &Blob::default());
        run.status();
        run.harness.device.answer = Some((characteristic, bytes));
        assert_eq!(run.run(command), Outcome::Failed(Failure::Protocol));
    }
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.raw = Some((uuids::GET_SYSTEM_STATUS, vec![0x00]));
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
    run.harness.device.raw = None;
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.device.key = Some([1; 32]);
    assert_eq!(
        run.run(Command::Status),
        Outcome::Failed(Failure::KeyRejected)
    );
    assert_eq!(run.engine.state(), EngineState::KeyRejected);
}

#[test]
fn a_stale_read_counter_is_adopted_not_refused() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.harness.device.pump_counter = 0;
    assert!(matches!(
        run.run(Command::Status),
        Outcome::Completed(Report::Status(_))
    ));
}

#[test]
fn a_combined_bolus_front_load_reports_through_the_second_immediate_pair() {
    let mut bytes = vec![1];
    bytes.extend([0; 12]);
    bytes.push(1);
    for field in [77i32, 100, 600, 100, 250, 0, 60] {
        bytes.extend(field.to_le_bytes());
    }
    let crc = crc16(&bytes);
    bytes.extend(crc.to_le_bytes());
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.answer = Some((uuids::GET_BOLUS_STATUS, bytes));
    let therapy = run.status().therapy;
    assert_eq!(
        therapy.bolus,
        Bolus::Immediate {
            requested: units(2_500),
            delivered: units(1_000),
        }
    );
    assert_eq!(
        therapy.extended,
        Extended::Running {
            amount: units(6_000),
            duration: Duration::from_secs(3600),
        }
    );
}

#[test]
fn refuses_a_pump_that_is_not_the_session_one() {
    let mut pump = sim();
    pump.settings.insert(setting::SERIAL_NUMBER_LOWER, 1);
    let mut run = Run::new(pump, &Blob::default());
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
    let mut pump = sim();
    pump.versions.pump_base = "0.9".into();
    let mut run = Run::new(pump, &Blob::default());
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
}

#[test]
fn walks_the_credential_ladder() {
    let wrong = (b"O2".to_vec(), [1; 16]);
    for (credentials, ok) in [
        (vec![wrong.clone(), (b"O2".to_vec(), PASSWORD)], true),
        (vec![wrong.clone()], false),
        (vec![(b"P1".to_vec(), PASSWORD)], true),
        (vec![(b"X1".to_vec(), PASSWORD)], true),
    ] {
        let blob = Blob {
            credentials,
            ..Blob::default()
        };
        let mut run = Run::new(sim(), &blob);
        let outcome = run.run(Command::Status);
        assert_eq!(matches!(outcome, Outcome::Completed(_)), ok, "{outcome:?}");
    }
    let mut run = Run::new(sim(), &Blob::default());
    let at = run.harness.transcript.len() + 8;
    run.harness.faults.insert(at, Fault::BleStatus(0x05));
    assert_eq!(
        run.run(Command::Status),
        Outcome::Failed(Failure::Status(0x05))
    );
}

#[cfg(feature = "delivery")]
#[test]
fn observes_what_the_pump_is_doing() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.run(tbr(150));
    run.run(extended(3_000));
    run.engine
        .submit(Command::Bolus {
            amount: units(10_000),
        })
        .unwrap();
    run.harness
        .advance(run.engine.as_mut(), Duration::from_secs(5))
        .unwrap();
    run.restart();
    let therapy = run.status().therapy;
    assert!(matches!(
        therapy.basal,
        Basal::Temporary {
            rate: TempBasalRate::Percent(percent),
            ..
        } if percent == Percent::new(150)
    ));
    assert!(matches!(therapy.bolus, Bolus::Immediate { .. }));
    assert_eq!(
        therapy.extended,
        Extended::Running {
            amount: units(3_000),
            duration: Duration::from_secs(3600)
        }
    );
    assert_eq!(run.status().therapy.extended, therapy.extended);
}

#[test]
fn an_unknown_bolus_step_falls_back_to_the_default() {
    let mut pump = sim();
    pump.settings.insert(setting::BOLUS_STEP_SELECTION, 7);
    let mut run = Run::new(pump, &Blob::default());
    run.status();
    let info = run
        .harness
        .events
        .iter()
        .find_map(|e| match e {
            Event::DeviceInfo(info) => Some(info.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(info.limits.bolus.min, units(50));
}

#[test]
fn a_missing_or_unreadable_session_leaves_the_engine_unpaired() {
    let corrupt = {
        let mut bytes = Blob::default().encode();
        bytes[10] ^= 1;
        bytes
    };
    for store in [None, Some(corrupt)] {
        let mut h = harness(sim(), &Blob::default());
        h.store.remove(&Slot::Session);
        if let Some(bytes) = store {
            h.store.insert(Slot::Session, bytes);
        }
        let mut run = Run {
            harness: h,
            engine: engine(),
        };
        run.settle();
        assert_eq!(run.engine.state(), EngineState::Unpaired);
        run.engine.handle_link_lost();
    }
    for step in 1..=4 {
        let mut h = harness(sim(), &Blob::default());
        h.faults.insert(step, Fault::HostFails);
        let mut run = Run {
            harness: h,
            engine: engine(),
        };
        run.settle();
        assert_eq!(run.engine.state(), EngineState::Unpaired);
    }
}

#[test]
fn keeps_the_host_contract() {
    let mut engine = engine();
    assert_eq!(engine.state(), EngineState::Loading);
    assert_eq!(
        engine.handle_completion(OpId::new(99), Completion::Stored),
        Err(CompletionError::NotPending)
    );
    let load = engine.poll_action().unwrap();
    assert_eq!(
        engine.handle_completion(load.id(), Completion::Stored),
        Err(CompletionError::Mismatch)
    );
    engine
        .handle_completion(
            load.id(),
            Completion::Loaded(Some(Blob::default().encode())),
        )
        .unwrap();
    let unwrap = engine.poll_action().unwrap();
    engine
        .handle_completion(
            unwrap.id(),
            Completion::Secret(Secret::new(Blob::default().encode())),
        )
        .unwrap();
    for _ in 0..2 {
        let load = engine.poll_action().unwrap();
        engine
            .handle_completion(load.id(), Completion::Loaded(None))
            .unwrap();
    }
    assert_eq!(engine.state(), EngineState::Paired);
    engine.therapy_mut().mode = PumpMode::Running;
    let first = engine.submit(Command::Status).unwrap();
    let connect = engine.poll_action().unwrap();
    assert!(matches!(connect, Action::Ble { .. }));
    engine
        .handle_completion(connect.id(), Completion::Failed)
        .unwrap();
    let queued: Vec<_> = (0..6).map(|_| engine.submit(Command::Status)).collect();
    assert!(queued[..5].iter().all(Result::is_ok));
    assert_eq!(queued[5], Err(pump_shared::SubmitError::QueueFull));
    let stop = engine.stop();
    engine.handle_link_lost();
    engine.handle_notification(pump_shared::Notification {
        service: uuids::CONTROL_SERVICE,
        characteristic: uuids::CONTROL_NOTIFY,
        data: vec![],
    });
    let finished: Vec<_> = std::iter::from_fn(|| engine.poll_event())
        .filter_map(|event| match event {
            Event::Finished { id, outcome } => Some((id, outcome)),
            _ => None,
        })
        .collect();
    assert_eq!(finished[0], (first, Outcome::Failed(Failure::Protocol)));
    assert_eq!(finished.len(), 6);
    assert!(finished[1..]
        .iter()
        .all(|(_, o)| *o == Outcome::Refused(Refusal::Busy)));
    assert!(!finished.iter().any(|(id, _)| *id == stop));
    assert_eq!(engine.poll_timeout(), None);
    engine.handle_timeout(Timestamp::from_millis(5));
    assert!(engine.poll_action().is_some());
}

#[test]
fn a_stop_before_anything_was_written_refuses_the_command() {
    let mut run = Run::new(sim(), &Blob::default());
    let id = run.engine.submit(Command::Status).unwrap();
    let stop = run.engine.stop();
    run.settle();
    assert_eq!(run.outcome(id), Some(Outcome::Refused(Refusal::Busy)));
    assert_eq!(run.outcome(stop), Some(Outcome::Completed(Report::Done)));
    assert_eq!(run.engine.therapy().bolus, Bolus::Idle);
}

#[test]
fn a_lost_link_is_reconnected() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    run.engine.handle_link_lost();
    let before = run.harness.transcript.len();
    run.status();
    assert!(run.harness.transcript[before].contains("Connect"));
}

#[test]
fn describes_the_driver() {
    let info = DRIVER.info();
    assert_eq!(info.id, "ypso");
    assert!(info.is_compatible());
    assert_eq!(info.has_feature("delivery"), cfg!(feature = "delivery"));
    assert_eq!(info.has_feature("opn"), cfg!(feature = "opn"));
    assert!(info.accepts(pump_shared::CommandKind::Stop));
    assert_eq!(
        info.accepts(pump_shared::CommandKind::Bolus),
        cfg!(feature = "delivery")
    );
    assert!(info.listened().next().is_none());
    assert_eq!(
        info.variants.len(),
        if cfg!(feature = "opn") { 2 } else { 1 }
    );
    let other = pump_shared::Advertisement {
        address: pump_shared::Address::new(MAC),
        name: Some("Other"),
        service_uuids: &[],
        manufacturer: None,
        rssi: -50,
    };
    assert!(DRIVER.matches(&other).is_none());
    assert_eq!(format!("{:?}", DRIVER), "Ypso");
}

#[test]
fn a_failure_while_learning_the_epoch_ends_the_command() {
    for gap in 1..=12 {
        let mut run = Run::new(sim(), &Blob::default());
        run.status();
        let first = run.harness.transcript.len() + 1;
        for at in first..first + 6 {
            run.harness.faults.insert(at, Fault::BleStatus(0x8B));
        }
        run.harness
            .faults
            .insert(first + 6 + gap, Fault::BleTimeout);
        let id = run.submit(Command::ReadBasalProfile);
        assert!(run.outcome(id).is_some());
    }
}

#[cfg(all(feature = "opn", feature = "delivery"))]
#[test]
fn an_opn_pump_takes_an_immediate_bolus_only() {
    let mut pump = opn();
    pump.versions.pump_base = "1.0".into();
    let mut run = Run::new(pump, &Blob::opn());
    run.status();
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_000)
        }),
        Outcome::Completed(Report::Delivered(units(1_000)))
    );
    assert_eq!(
        run.run(extended(2_000)),
        Outcome::Refused(Refusal::Unsupported)
    );
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
}

fn basal_max<D: SimDevice>(run: &Run<D>) -> BasalRate {
    run.harness
        .events
        .iter()
        .find_map(|e| match e {
            Event::DeviceInfo(info) => Some(info.limits.basal.max),
            _ => None,
        })
        .unwrap()
}

#[test]
fn a_negative_basal_cap_leaves_the_pump_maximum_in_place() {
    let mut pump = sim();
    pump.settings.insert(setting::BASAL_RATE_CAP, -1);
    pump.settings.insert(setting::BASAL_RATE_MAX, 2_000);
    let mut run = Run::new(pump, &Blob::default());
    run.status();
    assert_eq!(basal_max(&run), BasalRate::from_milliunits_per_hour(20_000));
}

#[test]
fn the_basal_cap_clamps_below_the_pump_maximum() {
    let mut pump = sim();
    pump.settings.insert(setting::BASAL_RATE_CAP, 1_000);
    pump.settings.insert(setting::BASAL_RATE_MAX, 2_500);
    let mut run = Run::new(pump, &Blob::default());
    run.status();
    assert_eq!(basal_max(&run), BasalRate::from_milliunits_per_hour(10_000));
}

#[test]
fn a_zero_basal_cap_is_a_literal_zero_ceiling() {
    let mut pump = sim();
    pump.settings.insert(setting::BASAL_RATE_CAP, 0);
    let mut run = Run::new(pump, &Blob::default());
    run.status();
    assert_eq!(basal_max(&run), BasalRate::from_milliunits_per_hour(0));
}

#[test]
fn no_reported_basal_limits_means_no_ceiling() {
    let mut pump = sim();
    pump.settings.insert(setting::BASAL_RATE_CAP, -1);
    pump.settings.insert(setting::BASAL_RATE_MIN, -1);
    pump.settings.insert(setting::BASAL_RATE_MAX, -1);
    let mut run = Run::new(pump, &Blob::default());
    run.status();
    assert!(basal_max(&run) >= BasalRate::from_milliunits_per_hour(1_000_000));
}

#[test]
fn a_clock_sync_near_midnight_does_not_fall_a_day_behind() {
    let mut pump = sim();
    pump.clock = 824_000_000;
    let mut run = Run::new(pump, &Blob::default());
    run.status();
    let midnight = 20_500 * 86_400u64;
    run.engine
        .handle_wall_clock(pump_shared::UtcTime::from_unix_millis(
            (midnight + 86_397) * 1000,
        ));
    assert_eq!(
        run.run(Command::SyncClock),
        Outcome::Completed(Report::Done)
    );
    let clock = match run.run(Command::ReadClock) {
        Outcome::Completed(Report::Clock(time)) => time.local_seconds(),
        other => panic!("{other:?}"),
    };
    assert!(clock >= midnight + 86_400, "stuck before midnight: {clock}");
    assert!(
        clock % 86_400 < 3_600,
        "not just after midnight: {}",
        clock % 86_400
    );
}

#[cfg(feature = "delivery")]
#[test]
fn a_temp_basal_the_pump_reports_already_running_is_busy() {
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.refuse = Some((uuids::START_STOP_TBR, 0xB0));
    assert_eq!(run.run(tbr(150)), Outcome::Refused(Refusal::Busy));
}

#[cfg(feature = "delivery")]
#[test]
fn an_unconfirmed_absolute_temp_basal_is_unknown() {
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.deaf = true;
    assert_eq!(run.run(absolute(3_000)), Outcome::Unknown);
}

#[cfg(feature = "delivery")]
#[test]
fn clearing_a_temp_basal_before_an_unconfirmed_one_is_unknown() {
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    assert_eq!(run.run(tbr(150)), Outcome::Completed(Report::Done));
    run.harness.device.deaf = true;
    assert_eq!(run.run(tbr(80)), Outcome::Unknown);
}

#[cfg(feature = "delivery")]
#[test]
fn a_basal_profile_write_that_fails_partway_is_unknown() {
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.refuse = Some((uuids::SETTING_VALUE, 0x82));
    let profile = BasalProfile::new(vec![BasalSegment {
        start: Duration::ZERO,
        rate: BasalRate::from_milliunits_per_hour(900),
    }])
    .unwrap();
    assert_eq!(
        run.run(Command::SetBasalProfile { profile }),
        Outcome::Unknown
    );
}

#[cfg(feature = "delivery")]
#[test]
fn a_dose_refused_while_the_pump_is_quietly_stopped_is_not_running() {
    let mut run = Run::new(sim(), &Blob::default());
    run.status();
    let now = run.harness.now;
    run.harness.device.stop(now);
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_000)
        }),
        Outcome::Refused(Refusal::NotRunning)
    );
}

#[test]
fn a_running_temp_basal_without_a_remaining_field_is_unknown() {
    let mut status = vec![10u8];
    status.extend(1_000i32.to_le_bytes());
    status.push(4);
    status.extend(800i32.to_le_bytes());
    status.extend(150i32.to_le_bytes());
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.answer = Some((uuids::GET_SYSTEM_STATUS, status));
    assert_eq!(run.status().therapy.basal, Basal::Unknown);
}

#[test]
fn a_system_status_with_a_bad_crc_is_a_protocol_error() {
    let mut status = vec![10u8];
    status.extend(1_000i32.to_le_bytes());
    status.push(4);
    status.extend(800i32.to_le_bytes());
    status.extend(100i32.to_le_bytes());
    status.extend(0i32.to_le_bytes());
    status.extend([0xFF, 0xFF]);
    let mut run = Run::new(pump(), &Blob::default());
    run.status();
    run.harness.device.answer = Some((uuids::GET_SYSTEM_STATUS, status));
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
}
