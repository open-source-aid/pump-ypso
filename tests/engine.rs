#![cfg(feature = "sim")]

mod e2e_support;

use e2e_support::scenarios::*;
use e2e_support::*;
use pump_testkit::Fault;

fn pump() -> Pump {
    Pump::new(sim())
}

#[test]
fn connects_and_reads_status_() {
    connects_and_reads_status(pump());
}

#[test]
fn reads_the_clock_() {
    reads_the_clock(pump());
}

#[test]
fn syncs_the_clock_() {
    syncs_the_clock(pump());
}

#[test]
fn reads_and_writes_the_basal_profile_() {
    reads_and_writes_the_basal_profile(pump());
}

#[test]
fn sets_and_cancels_a_temp_basal_() {
    sets_and_cancels_a_temp_basal(pump());
}

#[test]
fn refuses_what_the_pump_cannot_do_() {
    refuses_what_the_pump_cannot_do(pump());
}

#[test]
fn unpairs_() {
    unpairs(pump());
}

#[test]
fn survives_a_timeout_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::BleTimeout);
}

#[test]
fn survives_a_disconnect_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::BleDisconnected);
}

#[test]
fn survives_a_host_failure_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::HostFails);
}

#[test]
fn survives_a_counter_error_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::BleStatus(0x8B));
}

#[test]
fn a_wrong_key_is_key_rejected_() {
    a_wrong_key_is_key_rejected(pump());
}

#[test]
fn jumps_a_counter_moved_by_another_app_() {
    jumps_a_counter_moved_by_another_app(pump());
}

#[test]
fn gives_up_after_the_largest_jump_() {
    gives_up_after_the_largest_jump(pump());
}

#[test]
fn follows_a_pump_reboot_() {
    follows_a_pump_reboot(pump());
}

#[test]
fn learns_the_epoch_before_jumping_() {
    learns_the_epoch_before_jumping(pump());
}

#[test]
fn restarts_from_stored_slots_() {
    restarts_from_stored_slots(pump());
}

#[test]
fn restores_old_storage_() {
    restores_old_storage(pump());
}

#[test]
fn ignores_notifications_() {
    ignores_notifications(pump());
}

#[cfg(feature = "opn")]
#[test]
fn drives_an_opn_pump_in_clear_() {
    drives_an_opn_pump_in_clear(Pump::new(opn()));
}

#[cfg(feature = "opn")]
#[test]
fn never_drives_an_encrypted_pump_in_clear_() {
    never_drives_an_encrypted_pump_in_clear(pump());
}

#[test]
fn boluses_() {
    boluses(pump());
}

#[test]
fn stop_preempts_a_bolus_() {
    stop_preempts_a_bolus(pump());
}

#[test]
fn a_timed_out_bolus_is_unknown_and_never_resent_() {
    a_timed_out_bolus_is_unknown_and_never_resent(pump());
}

#[test]
fn extended_bolus_() {
    extended_bolus(pump());
}

#[test]
fn reads_history_() {
    reads_history(pump());
}

#[test]
fn every_command_survives_timeouts() {
    sweeps_every_command(pump, Fault::BleTimeout);
}

#[test]
fn every_command_survives_disconnects() {
    sweeps_every_command(pump, Fault::BleDisconnected);
}

#[test]
fn every_command_survives_counter_errors() {
    sweeps_every_command(pump, Fault::BleStatus(0x8B));
}

#[test]
fn every_command_survives_refusals() {
    sweeps_every_command(pump, Fault::BleStatus(0x87));
}

#[test]
fn every_command_survives_host_failures() {
    sweeps_every_command(pump, Fault::HostFails);
}

use std::time::Duration;

use pump_shared::{
    Action, Basal, Bolus, Command, Completion, CompletionError, Driver, EngineState, Event,
    Extended, Failure, HistoryEvent, OpId, Outcome, Percent, PumpMode, Refusal, Report, Secret,
    Sequence, Slot, TempBasalRate,
};
use pump_ypso::protocol::uuids;
use pump_ypso::DRIVER;

fn status(run: &mut Run<Pump>) -> pump_shared::PumpStatus {
    match run.run(Command::Status) {
        Outcome::Completed(Report::Status(status)) => status,
        other => panic!("{other:?}"),
    }
}

fn history(run: &mut Run<Pump>) -> Outcome {
    run.run(Command::ReadHistory {
        after: Sequence::new(0),
    })
}

fn tbr(percent: u16) -> Command {
    Command::TempBasal {
        rate: TempBasalRate::Percent(Percent::new(percent)),
        duration: Duration::from_secs(1800),
    }
}

#[cfg(feature = "delivery")]
#[test]
fn a_pump_that_cannot_deliver_refuses_the_dose() {
    let mut run = Run::new(pump(), &Blob::default());
    status(&mut run);
    run.harness.device.refuse_bolus = Some(0x87);
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_000)
        }),
        Outcome::Refused(Refusal::NotRunning)
    );
    run.harness.device.sim.delivery_state = 3;
    assert_eq!(status(&mut run).therapy.mode, PumpMode::Suspended);
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
    status(&mut run);
    run.harness.device.deaf = true;
    assert_eq!(run.run(tbr(150)), Outcome::Unknown);
    run.harness.device.deaf = false;
    run.run(Command::ExtendedBolus {
        amount: units(2_000),
        duration: Duration::from_secs(3600),
    });
    run.harness.device.deaf = true;
    assert_eq!(run.run(Command::CancelExtendedBolus), Outcome::Unknown);
    run.harness.device.bolus.extended_sequence = 0;
    run.harness.device.quiet = true;
    assert_eq!(
        run.run(Command::Bolus { amount: units(500) }),
        Outcome::Unknown
    );
    run.harness.device.quiet = false;
    status(&mut run);
    assert_eq!(
        run.run(Command::ExtendedBolus {
            amount: units(2_000),
            duration: Duration::from_secs(3600),
        }),
        Outcome::Unknown
    );
    let profile = pump_shared::BasalProfile::new(vec![pump_shared::BasalSegment {
        start: Duration::ZERO,
        rate: pump_shared::BasalRate::from_milliunits_per_hour(900),
    }])
    .unwrap();
    assert_eq!(
        run.run(Command::SetBasalProfile { profile }),
        Outcome::Unknown
    );
    run.harness.device.deaf = false;
    run.harness.device.refuse_bolus = Some(0x81);
    status(&mut run);
    assert_eq!(
        run.run(Command::ExtendedBolus {
            amount: units(2_000),
            duration: Duration::from_secs(3600),
        }),
        Outcome::Failed(Failure::Status(0x81))
    );
}

#[test]
fn decodes_every_known_history_entry() {
    let mut device = pump();
    for (kind, values) in [
        (3, [200, 60, 0]),
        (1, [200, 60, 0]),
        (18, [300, 100, 90]),
        (17, [300, 100, 90]),
        (30, [150, 0, 0]),
        (32, [0, 0, 0]),
        (7, [3, 50, 40]),
        (14, [10, 0, 0]),
        (16, [0, 0, 0]),
        (24, [0, 0, 0]),
        (25, [0, 0, 0]),
        (29, [139, 0, 0]),
    ] {
        device.log(kind, values);
    }
    let mut run = Run::new(device, &Blob::default());
    let Outcome::Completed(Report::History(entries)) = history(&mut run) else {
        panic!()
    };
    let events: Vec<_> = entries.iter().map(|e| e.event).collect();
    assert_eq!(events.len(), 12);
    assert!(
        matches!(events[0], HistoryEvent::Bolus { delivered, .. } if delivered == units(1_390))
    );
    assert_eq!(
        events[1],
        HistoryEvent::Replaced(pump_shared::Component::Cannula)
    );
    assert_eq!(
        events[2],
        HistoryEvent::Replaced(pump_shared::Component::Battery)
    );
    assert_eq!(events[3], HistoryEvent::Rewound);
    assert_eq!(events[4], HistoryEvent::Resumed);
    assert_eq!(events[5], HistoryEvent::BasalProfileChanged);
    assert_eq!(events[6], HistoryEvent::TempBasalEnded);
    assert_eq!(
        events[7],
        HistoryEvent::ExtendedBolusEnded {
            delivered: units(1_500)
        }
    );
    assert!(
        matches!(events[8], HistoryEvent::Bolus { in_progress: true, duration, .. } if duration == Duration::from_secs(5400))
    );
    assert!(matches!(
        events[9],
        HistoryEvent::Bolus {
            in_progress: false,
            ..
        }
    ));
    assert!(matches!(
        events[10],
        HistoryEvent::Bolus {
            in_progress: true,
            ..
        }
    ));
    assert!(
        matches!(events[11], HistoryEvent::Bolus { requested, .. } if requested == units(2_000))
    );
}

#[test]
fn skips_corrupt_entries_and_restarts_once_when_the_log_moves() {
    let mut device = pump();
    for _ in 0..4 {
        device.log(16, [0, 0, 0]);
    }
    device.corrupt = Some(2);
    let mut run = Run::new(device, &Blob::default());
    let outcome = history(&mut run);
    let Outcome::Completed(Report::History(entries)) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(entries.len(), 3);
    run.harness.device.corrupt = None;
    run.harness.device.grow = vec![1];
    let outcome = history(&mut run);
    let Outcome::Completed(Report::History(entries)) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(entries.len(), 4);
    run.harness.device.grow = vec![2];
    let outcome = history(&mut run);
    let Outcome::Completed(Report::History(entries)) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(entries[0].sequence.get(), 107);
    run.harness.device.grow = vec![2, 2];
    assert_eq!(history(&mut run), Outcome::Failed(Failure::Protocol));
    run.harness.device.grow = vec![];
    run.harness.device.bad_index = true;
    assert_eq!(history(&mut run), Outcome::Failed(Failure::Protocol));
}

#[test]
fn an_older_history_service_selects_entries_by_a_bare_index() {
    let mut sim = sim();
    sim.versions.history = "1.1".into();
    let mut device = Pump::new(sim);
    device.log(16, [0, 0, 0]);
    device.log(16, [0, 0, 0]);
    device.corrupt = Some(0);
    let mut run = Run::new(device, &Blob::default());
    let Outcome::Completed(Report::History(entries)) = history(&mut run) else {
        panic!()
    };
    assert_eq!(entries.len(), 1);
}

#[test]
fn waits_out_the_basal_window_on_older_control_services() {
    let mut sim = sim();
    sim.versions.control = "1.2".into();
    sim.clock = NOON - 60 + 175;
    let mut run = Run::new(Pump::new(sim), &Blob::default());
    status(&mut run);
    let start = run.harness.now;
    if cfg!(feature = "delivery") {
        assert_eq!(run.run(tbr(150)), Outcome::Completed(Report::Done));
        assert!(run.harness.now.as_millis() - start.as_millis() >= 5_000);
    }
    run.engine
        .handle_wall_clock(pump_shared::UtcTime::from_unix_millis(1_000));
    assert_eq!(
        run.run(Command::SyncClock),
        Outcome::Completed(Report::Done)
    );
}

#[test]
fn rereads_the_clock_just_after_midnight() {
    let mut sim = sim();
    sim.clock = 9497 * 86_400 - 115;
    let mut run = Run::new(Pump::new(sim), &Blob::default());
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
        status(&mut run);
        run.harness.device.answer = Some((characteristic, bytes));
        assert_eq!(run.run(command), Outcome::Failed(Failure::Protocol));
    }
    let mut run = Run::new(pump(), &Blob::default());
    status(&mut run);
    run.harness.device.raw = Some((uuids::GET_SYSTEM_STATUS, vec![0x00]));
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
    run.harness.device.raw = None;
    run.harness.device.sim.pump_counter = 0;
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
    let mut run = Run::new(pump(), &Blob::default());
    status(&mut run);
    run.harness.device.sim.key = [1; 32];
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
}

#[test]
fn refuses_a_pump_that_is_not_the_session_one() {
    let mut sim = sim();
    sim.settings
        .insert(pump_ypso::protocol::setting::SERIAL_NUMBER_LOWER, 1);
    let mut run = Run::new(Pump::new(sim), &Blob::default());
    assert_eq!(run.run(Command::Status), Outcome::Failed(Failure::Protocol));
    let mut sim = e2e_support::sim();
    sim.versions.pump_base = "0.9".into();
    let mut run = Run::new(Pump::new(sim), &Blob::default());
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
        let mut run = Run::new(pump(), &blob);
        let outcome = run.run(Command::Status);
        assert_eq!(matches!(outcome, Outcome::Completed(_)), ok, "{outcome:?}");
    }
    let mut run = Run::new(pump(), &Blob::default());
    let at = run.harness.transcript.len() + 8;
    run.harness
        .faults
        .insert(at, pump_testkit::Fault::BleStatus(0x05));
    assert_eq!(
        run.run(Command::Status),
        Outcome::Failed(Failure::Status(0x05))
    );
}

#[test]
fn observes_what_the_pump_is_doing() {
    let mut device = pump();
    device.sim.tbr_percent = 150;
    device.bolus.status = 1;
    device.bolus.total = 1_000;
    device.bolus.extended_sequence = 4;
    device.bolus.extended_total = 300;
    device.bolus.extended_minutes = 60;
    let mut run = Run::new(device, &Blob::default());
    let therapy = status(&mut run).therapy;
    assert_eq!(therapy.basal, Basal::Unknown);
    assert!(matches!(therapy.bolus, Bolus::Immediate { .. }));
    assert_eq!(
        therapy.extended,
        Extended::Running {
            amount: units(3_000),
            duration: Duration::from_secs(3600)
        }
    );
    assert_eq!(status(&mut run).therapy.extended, therapy.extended);
}

#[test]
fn an_unknown_bolus_step_falls_back_to_the_default() {
    let mut sim = sim();
    sim.settings
        .insert(pump_ypso::protocol::setting::BOLUS_STEP_SELECTION, 7);
    let mut run = Run::new(Pump::new(sim), &Blob::default());
    status(&mut run);
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
        let mut h = harness(pump(), &Blob::default());
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
        let mut h = harness(pump(), &Blob::default());
        h.faults.insert(step, pump_testkit::Fault::HostFails);
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
    let events: Vec<_> = std::iter::from_fn(|| engine.poll_event()).collect();
    let finished = finished(&events);
    assert_eq!(finished[0], (first, Outcome::Failed(Failure::Protocol)));
    assert_eq!(finished.len(), 6);
    assert!(finished[1..]
        .iter()
        .all(|(_, o)| *o == Outcome::Refused(Refusal::Busy)));
    assert!(!finished.iter().any(|(id, _)| *id == stop));
    assert_eq!(engine.poll_timeout(), None);
    engine.handle_timeout(pump_shared::Timestamp::from_millis(5));
    assert!(engine.poll_action().is_some());
}

#[test]
fn a_stop_before_anything_was_written_refuses_the_command() {
    let mut run = Run::new(pump(), &Blob::default());
    let id = run.engine.submit(Command::Status).unwrap();
    let stop = run.engine.stop();
    run.settle();
    assert_eq!(run.outcome(id), Some(Outcome::Refused(Refusal::Busy)));
    assert_eq!(run.outcome(stop), Some(Outcome::Completed(Report::Done)));
    assert_eq!(run.engine.therapy().bolus, Bolus::Idle);
}

#[test]
fn a_lost_link_is_reconnected() {
    let mut run = Run::new(pump(), &Blob::default());
    status(&mut run);
    run.engine.handle_link_lost();
    let before = run.harness.transcript.len();
    status(&mut run);
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
fn every_command_survives_timeouts_on_an_older_control_service() {
    sweeps_every_command(
        || {
            let mut sim = sim();
            sim.versions.control = "1.2".into();
            Pump::new(sim)
        },
        Fault::BleTimeout,
    );
}

#[test]
fn a_failure_while_learning_the_epoch_ends_the_command() {
    for gap in 1..=12 {
        let mut run = Run::new(pump(), &Blob::default());
        status(&mut run);
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
    let blob = Blob {
        version: 4,
        flags: 1,
        ..Blob::default()
    };
    let mut sim = opn();
    sim.versions.pump_base = "1.0".into();
    let mut run = Run::new(Pump::new(sim), &blob);
    status(&mut run);
    assert_eq!(
        run.run(Command::Bolus {
            amount: units(1_000)
        }),
        Outcome::Completed(Report::Delivered(units(1_000)))
    );
    assert_eq!(
        run.run(Command::ExtendedBolus {
            amount: units(2_000),
            duration: Duration::from_secs(3600),
        }),
        Outcome::Refused(Refusal::Unsupported)
    );
    assert_eq!(
        run.run(Command::CancelExtendedBolus),
        Outcome::Completed(Report::Done)
    );
}
