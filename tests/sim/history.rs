use pump_shared::BleResult;
use pump_ypso::protocol::Characteristic;
use pump_ypso::sim::{crc16, Entry, Log, YpsoSim};

use super::{safevar, App};

const LOGS: [(Characteristic, Characteristic, Characteristic); 4] = [
    (
        Characteristic::EventEntryCount,
        Characteristic::EventEntryIndex,
        Characteristic::EventEntryValue,
    ),
    (
        Characteristic::AlarmEntryCount,
        Characteristic::AlarmEntryIndex,
        Characteristic::AlarmEntryValue,
    ),
    (
        Characteristic::SystemEntryCount,
        Characteristic::SystemEntryIndex,
        Characteristic::SystemEntryValue,
    ),
    (
        Characteristic::ComplaintEntryCount,
        Characteristic::ComplaintEntryIndex,
        Characteristic::ComplaintEntryValue,
    ),
];

fn three_entries() -> Log {
    let mut log = Log::default();
    for (kind, stamp) in [(215u8, 100u32), (204, 200), (14, 300)] {
        log.push(stamp, kind, [u16::from(kind), 2, 3]);
    }
    log
}

fn fill(sim: &mut YpsoSim) {
    sim.events = three_entries();
    sim.alarms = three_entries();
    sim.system = three_entries();
    sim.complaints = three_entries();
}

fn entry_bytes(stamp: u32, kind: u8, values: [u16; 3], sequence: u32, index: u16) -> Vec<u8> {
    let mut bytes = stamp.to_le_bytes().to_vec();
    bytes.push(kind);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&index.to_le_bytes());
    let crc = crc16(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes
}

#[test]
fn the_crc_is_gen_crc16_and_matches_the_golden_vector() {
    let data: Vec<u8> = (0..=0x10u8).collect();
    assert_eq!(crc16(&data).to_le_bytes(), [0x1E, 0x8F]);
    assert_ne!(crc16(&data), crc16(&data[..16]));
}

#[test]
fn every_log_reads_newest_first_with_auto_increment_then_refuses_past_the_end() {
    let mut app = App::connected();
    fill(&mut app.sim);
    for (count, _, value) in LOGS {
        assert_eq!(app.get(count), Ok(safevar(3)));
        for (index, (stamp, kind, sequence)) in [(300, 14, 3), (200, 204, 2), (100, 215, 1)]
            .into_iter()
            .enumerate()
        {
            let values = [u16::from(kind), 2, 3];
            assert_eq!(
                app.get(value),
                Ok(entry_bytes(stamp, kind, values, sequence, index as u16))
            );
        }
        assert_eq!(app.get(value), Err(BleResult::Status(0x84)));
    }
}

#[test]
fn an_index_write_selects_an_entry_and_out_of_range_is_0x82() {
    let mut app = App::connected();
    fill(&mut app.sim);
    for (_, index, value) in LOGS {
        assert_eq!(app.send(index, &safevar(2)), BleResult::Done);
        assert_eq!(app.get(value).unwrap()[15..17], [2, 0]);
        assert_eq!(app.send(index, &safevar(3)), BleResult::Status(0x82));
        assert_eq!(app.send(index, &safevar(-1)), BleResult::Status(0x82));
        assert_eq!(app.send(index, &[1, 0]), BleResult::Status(0x81));
        assert_eq!(app.send(index, &safevar(0)), BleResult::Done);
        assert_eq!(app.get(value).unwrap()[15..17], [0, 0]);
    }
}

#[test]
fn a_history_service_before_1_2_takes_a_bare_u16_index() {
    let mut app = App::connected();
    app.sim.versions.history = "1.1".into();
    fill(&mut app.sim);
    assert_eq!(
        app.send(Characteristic::EventEntryIndex, &[1, 0]),
        BleResult::Done
    );
    assert_eq!(app.get(Characteristic::EventEntryValue).unwrap()[4], 204);
    assert_eq!(
        app.send(Characteristic::EventEntryIndex, &[3, 0]),
        BleResult::Status(0x82)
    );
    assert_eq!(
        app.send(Characteristic::EventEntryIndex, &safevar(1)),
        BleResult::Status(0x81)
    );
}

#[test]
fn an_entry_is_nineteen_bytes_in_four_frames() {
    let mut app = App::connected();
    fill(&mut app.sim);
    let (service, uuid) = super::ids(Characteristic::EventEntryValue);
    assert_eq!(app.read_frames(service, uuid).unwrap().len(), 4);
    assert_eq!(app.get(Characteristic::EventEntryValue).unwrap().len(), 19);
}

#[test]
fn a_count_may_report_capacity_and_a_read_past_the_fill_is_0x84() {
    let mut app = App::connected();
    app.sim.events = three_entries();
    app.sim.events.capacity = Some(10);
    assert_eq!(app.get(Characteristic::EventEntryCount), Ok(safevar(10)));
    assert_eq!(
        app.send(Characteristic::EventEntryIndex, &safevar(5)),
        BleResult::Done
    );
    assert_eq!(
        app.get(Characteristic::EventEntryValue),
        Err(BleResult::Status(0x84))
    );
}

#[test]
fn logs_are_scenario_knobs() {
    let mut log = Log::default();
    assert_eq!(log.push(7, 23, [1, 2, 3]), 1);
    assert_eq!(
        log.entries,
        [Entry {
            timestamp: 7,
            kind: 23,
            values: [1, 2, 3],
            sequence: 1
        }]
    );
    assert_eq!(log.clone(), log);
}
