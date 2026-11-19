use pump_shared::BleResult;
use pump_ypso::protocol::{assemble_serial, setting, Characteristic};

use super::{safevar, App};

const JAN_1_2022_01_09_03: i64 = 8036 * 86_400 + 3600 + 9 * 60 + 3;

#[test]
fn the_serial_is_served_lower_then_upper_as_the_handover_example() {
    let mut app = App::connected();
    let lower = app.setting(setting::SERIAL_NUMBER_LOWER);
    let upper = app.setting(setting::SERIAL_NUMBER_UPPER);
    assert_eq!((lower, upper), (63_011, 154));
    assert_eq!(assemble_serial(upper as u16, lower as u16), 10_155_555);
}

#[test]
fn the_defaults_cover_every_setting_the_driver_reads() {
    let mut app = App::connected();
    for (id, value) in [
        (setting::BASAL_PROFILE_SELECTION, 3),
        (setting::PUMP_MODE_SELECTION, 10),
        (setting::BOLUS_STEP_SELECTION, 10),
        (setting::basal_a(0), 52),
        (setting::basal_b(23), 40),
        (pump_ypso::sim::BOLUS_AMOUNT_CAP, 2_500),
        (setting::BASAL_RATE_CAP, 4_000),
        (setting::BASAL_RATE_MIN, 2),
        (setting::BASAL_RATE_MAX, 4_000),
        (setting::TBR_LONG_SCALE_MAX, 200),
        (setting::TBR_LONG_DURATION_MAX, 240),
        (setting::TBR_SHORT_SCALE_MAX, 200),
        (setting::TBR_SHORT_DURATION_MAX, 240),
        (setting::TBR_END_NOT_INDICATED, 1),
    ] {
        assert_eq!(app.setting(id), value, "setting {id}");
    }
    app.sim.settings.insert(setting::REMAINING_LIFETIME, 5);
    assert_eq!(app.setting(setting::REMAINING_LIFETIME), 5);
}

#[test]
fn a_selection_is_checked_when_its_value_is_read() {
    let mut app = App::connected();
    for id in [999, -1, 0, 123] {
        assert_eq!(
            app.send(Characteristic::SettingId, &safevar(id)),
            BleResult::Done
        );
        assert_eq!(
            app.get(Characteristic::SettingValue),
            Err(BleResult::Status(0x83))
        );
        assert_eq!(
            app.send(Characteristic::SettingValue, &safevar(1)),
            BleResult::Status(0x83)
        );
    }
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(122)),
        BleResult::Done
    );
    assert_eq!(app.get(Characteristic::SettingValue), Ok(safevar(0)));
    let mut corrupt = safevar(62);
    corrupt[7] ^= 1;
    assert_eq!(
        app.send(Characteristic::SettingId, &corrupt),
        BleResult::Status(0x81)
    );
    assert_eq!(
        app.get(Characteristic::SettingValue),
        Err(BleResult::Status(0x83))
    );
    assert_eq!(
        app.send(Characteristic::SettingId, &[62, 0]),
        BleResult::Status(0x81)
    );
}

#[test]
fn a_counter_selection_is_checked_at_once() {
    let mut app = App::connected();
    for (payload, code) in [
        (safevar(999), 0x83),
        (safevar(-1), 0x83),
        (vec![1, 0], 0x81),
    ] {
        assert_eq!(
            app.send(Characteristic::CounterId, &payload),
            BleResult::Status(code)
        );
    }
    assert_eq!(
        app.get(Characteristic::CounterValue),
        Err(BleResult::Failed)
    );
}

#[test]
fn a_reconnect_forgets_the_selected_setting() {
    let mut app = App::connected();
    assert_eq!(app.setting(setting::SERIAL_NUMBER_LOWER), 63_011);
    assert_eq!(app.op(super::connect()), BleResult::Done);
    assert_eq!(app.authorise(&super::PASSWORD), BleResult::Done);
    assert_eq!(
        app.get(Characteristic::SettingValue),
        Err(BleResult::Status(0x83))
    );
}

#[test]
fn a_basal_slot_write_changes_the_profile_and_logs_the_change() {
    let mut app = App::connected();
    app.now = 3_000;
    assert_eq!(
        app.send(
            Characteristic::SettingId,
            &safevar(i32::from(setting::basal_a(5)))
        ),
        BleResult::Done
    );
    assert_eq!(
        app.send(Characteristic::SettingValue, &safevar(80)),
        BleResult::Done
    );
    assert_eq!(app.setting(setting::basal_a(5)), 80);
    assert_eq!(
        app.send(
            Characteristic::SettingId,
            &safevar(i32::from(setting::basal_b(23)))
        ),
        BleResult::Done
    );
    assert_eq!(
        app.send(Characteristic::SettingValue, &safevar(10)),
        BleResult::Done
    );
    assert_eq!(app.events(), [(7, [5, 80, 52]), (8, [23, 10, 40])]);
    assert_eq!(app.sim.events.entries[0].timestamp, 3);
    assert_eq!(app.sim.events.entries[1].sequence, 2);
}

#[test]
fn another_setting_write_is_stored_without_a_history_entry() {
    let mut app = App::connected();
    assert_eq!(
        app.send(
            Characteristic::SettingId,
            &safevar(i32::from(setting::BASAL_PROFILE_SELECTION))
        ),
        BleResult::Done
    );
    let mut corrupt = safevar(4);
    corrupt[0] ^= 1;
    assert_eq!(
        app.send(Characteristic::SettingValue, &corrupt),
        BleResult::Status(0x81)
    );
    assert_eq!(
        app.send(Characteristic::SettingValue, &safevar(4)),
        BleResult::Done
    );
    assert_eq!(app.setting(setting::BASAL_PROFILE_SELECTION), 4);
    assert!(app.events().is_empty());
}

#[test]
fn counters_are_selected_then_read() {
    let mut app = App::connected();
    app.sim.counters.insert(17, 1234);
    assert_eq!(
        app.send(Characteristic::CounterId, &safevar(17)),
        BleResult::Done
    );
    assert_eq!(app.get(Characteristic::CounterValue), Ok(safevar(1234)));
    assert_eq!(
        app.send(Characteristic::CounterId, &safevar(18)),
        BleResult::Status(0x83)
    );
    app.sim.counters.clear();
    assert_eq!(
        app.get(Characteristic::CounterValue),
        Err(BleResult::Failed)
    );
}

#[test]
fn date_and_time_match_the_hardware_example() {
    let mut app = App::connected();
    app.sim.clock = JAN_1_2022_01_09_03;
    let (service, uuid) = super::ids(Characteristic::SystemDate);
    assert_eq!(app.read_frames(service, uuid).unwrap().len(), 3);
    assert_eq!(
        app.get(Characteristic::SystemDate),
        Ok(vec![0xE6, 0x07, 0x01, 0x01])
    );
    assert_eq!(
        app.get(Characteristic::SystemTime),
        Ok(vec![0x01, 0x09, 0x03])
    );
}

#[test]
fn the_pump_clock_runs_on_virtual_time() {
    let mut app = App::connected();
    app.sim.clock = JAN_1_2022_01_09_03;
    app.now = (59 * 86_400 + 22 * 3600 + 50 * 60 + 57) * 1000 + 999;
    assert_eq!(
        app.get(Characteristic::SystemDate),
        Ok(vec![0xE6, 0x07, 0x03, 0x02])
    );
    assert_eq!(app.get(Characteristic::SystemTime), Ok(vec![0, 0, 0]));
    app.sim.clock = 0;
    app.now = 8825 * 86_400 * 1000;
    assert_eq!(
        app.get(Characteristic::SystemDate),
        Ok(vec![0xE8, 0x07, 0x02, 0x1D])
    );
}

#[test]
fn a_clock_write_moves_the_pump_clock_and_logs_where_it_went() {
    let mut app = App::connected();
    app.sim.clock = JAN_1_2022_01_09_03;
    app.now = 500;
    assert_eq!(
        app.send(Characteristic::SystemTime, &[23, 59, 58]),
        BleResult::Done
    );
    assert_eq!(
        app.send(Characteristic::SystemDate, &[0xE8, 0x07, 0x02, 0x1D]),
        BleResult::Done
    );
    assert_eq!(app.get(Characteristic::SystemTime), Ok(vec![23, 59, 58]));
    assert_eq!(
        app.get(Characteristic::SystemDate),
        Ok(vec![0xE8, 0x07, 0x02, 0x1D])
    );
    assert_eq!(
        app.events(),
        [(13, [23, 59, 58]), (23, [0, 0, 0]), (12, [29, 2, 24])]
    );
    let stamps: Vec<u32> = app.sim.events.entries.iter().map(|e| e.timestamp).collect();
    let before_date = (8036 * 86_400 + 86_398) as u32;
    assert_eq!(
        stamps,
        [JAN_1_2022_01_09_03 as u32, before_date, before_date]
    );
    app.wait(2_500);
    assert_eq!(
        app.get(Characteristic::SystemDate),
        Ok(vec![0xE8, 0x07, 0x03, 0x01])
    );
}

#[test]
fn an_impossible_date_or_time_is_0x82() {
    let mut app = App::connected();
    for time in [&[24, 0, 0][..], &[0, 60, 0], &[0, 0, 60], &[1, 2]] {
        assert_eq!(
            app.send(Characteristic::SystemTime, time),
            BleResult::Status(0x82)
        );
    }
    for date in [
        &[0xE6, 0x07, 0x02, 0x1E][..],
        &[0xE6, 0x07, 0x0D, 0x01],
        &[0xE6, 0x07, 0x00, 0x01],
        &[0xCF, 0x07, 0x01, 0x01],
        &[0xE6, 0x07, 0x01],
    ] {
        assert_eq!(
            app.send(Characteristic::SystemDate, date),
            BleResult::Status(0x82)
        );
    }
    assert!(app.events().is_empty());
}
