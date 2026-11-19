use pump_shared::BleResult;
use pump_ypso::protocol::{setting, uuids, Characteristic};
use pump_ypso::sim::{crc16, Alarm};

use super::{bolus, le_i32, tbr, App};

const MINUTE: u64 = 60_000;

fn fields(bytes: &[u8], offsets: &[usize]) -> Vec<i32> {
    offsets.iter().map(|at| le_i32(bytes, *at)).collect()
}

fn immediate(app: &mut App) -> (u8, Vec<i32>) {
    let status = app.bolus_status();
    (status[0], fields(&status, &[1, 5, 9]))
}

fn slow(app: &mut App) -> (u8, Vec<i32>) {
    let status = app.bolus_status();
    (status[13], fields(&status, &[14, 18, 22, 26, 30, 34, 38]))
}

fn start(app: &mut App, payload: Vec<u8>) {
    assert_eq!(
        app.send(Characteristic::StartStopBolus, &payload),
        BleResult::Done
    );
}

fn temp(app: &mut App, percent: i32, minutes: i32) -> BleResult {
    app.send(Characteristic::StartStopTbr, &tbr(percent, minutes))
}

#[test]
fn an_immediate_bolus_runs_in_virtual_time_and_ends_in_its_running_record() {
    let mut app = App::connected();
    start(&mut app, bolus(100, 0, 0, 1));
    assert_eq!(app.events(), [(19, [100, 0, 0])]);
    assert_eq!(immediate(&mut app), (1, vec![1, 0, 100]));
    app.wait(870);
    assert_eq!(immediate(&mut app), (1, vec![1, 10, 100]));
    app.wait(7_830);
    assert_eq!(immediate(&mut app), (4, vec![1, 100, 100]));
    assert_eq!(app.events(), [(2, [100, 0, 0])]);
    assert_eq!(app.sim.reservoir, 14_900);
}

#[test]
fn the_bolus_status_is_forty_four_bytes_with_its_crc_in_six_frames() {
    let mut app = App::connected();
    let frames = app
        .read_frames(uuids::CONTROL_SERVICE, uuids::GET_BOLUS_STATUS)
        .unwrap();
    assert_eq!(
        frames.iter().map(|f| f[0]).collect::<Vec<_>>(),
        [0x16, 0x26, 0x36, 0x46, 0x56, 0x66]
    );
    let status = app.bolus_status();
    assert_eq!(status.len(), 44);
    assert_eq!(status[42..], crc16(&status[..42]).to_le_bytes());
}

#[test]
fn a_zero_bolus_stops_the_running_one_and_the_history_keeps_what_went_in() {
    let mut app = App::connected();
    start(&mut app, bolus(500, 0, 0, 1));
    app.wait(139 * 87);
    start(&mut app, bolus(0, 0, 0, 1));
    assert_eq!(immediate(&mut app), (4, vec![1, 139, 500]));
    assert_eq!(app.events(), [(2, [139, 0, 0])]);
    start(&mut app, bolus(0, 0, 0, 1));
    app.wait(MINUTE);
    assert_eq!(immediate(&mut app), (4, vec![1, 139, 500]));
    assert_eq!(app.sim.reservoir, 15_000 - 139);
}

#[test]
fn the_pump_refuses_a_bolus_with_its_own_codes() {
    let mut app = App::connected();
    for (payload, code) in [
        (bolus(9, 0, 0, 1), 0x82),
        (bolus(2_501, 0, 0, 1), 0x82),
        (bolus(100, 0, 0, 3), 0x82),
        (bolus(0, 0, 0, 0), 0x82),
        (bolus(100, 0, 0, 1)[..14].to_vec(), 0x81),
    ] {
        assert_eq!(
            app.send(Characteristic::StartStopBolus, &payload),
            BleResult::Status(code)
        );
    }
    let mut corrupt = bolus(100, 0, 0, 1);
    corrupt[13] ^= 1;
    assert_eq!(
        app.send(Characteristic::StartStopBolus, &corrupt),
        BleResult::Status(0x81)
    );
    start(&mut app, bolus(100, 0, 0, 1));
    assert_eq!(
        app.send(Characteristic::StartStopBolus, &bolus(100, 0, 0, 1)),
        BleResult::Status(0x87)
    );
    assert_eq!(app.events(), [(19, [100, 0, 0])]);
}

#[test]
fn a_bolus_needs_a_running_pump_a_cartridge_and_no_alarm() {
    let mut app = App::connected();
    let refused = |app: &mut App| {
        for payload in [bolus(100, 0, 0, 1), bolus(100, 30, 0, 2)] {
            assert_eq!(
                app.send(Characteristic::StartStopBolus, &payload),
                BleResult::Status(0x87)
            );
        }
    };
    app.sim.stop(app.at());
    refused(&mut app);
    app.sim.run(app.at());
    app.sim.reservoir = 0;
    refused(&mut app);
    app.sim.reservoir = 15_000;
    app.sim.alarm(Alarm::Occlusion, app.at());
    app.sim.run(app.at());
    refused(&mut app);
    app.sim.acknowledge_alarms();
    start(&mut app, bolus(100, 0, 0, 1));
}

#[test]
fn an_extended_bolus_spreads_over_its_minutes_and_ends_in_place() {
    let mut app = App::connected();
    start(&mut app, bolus(600, 60, 0, 2));
    assert_eq!(app.events(), [(1, [600, 60, 0])]);
    assert_eq!(slow(&mut app), (1, vec![1, 0, 600, 0, 0, 0, 60]));
    app.wait(30 * MINUTE);
    assert_eq!(slow(&mut app), (1, vec![1, 300, 600, 0, 0, 30, 60]));
    assert_eq!(immediate(&mut app), (0, vec![0, 0, 0]));
    app.wait(30 * MINUTE);
    assert_eq!(slow(&mut app), (0, vec![0; 7]));
    assert_eq!(app.events(), [(3, [600, 60, 0])]);
    assert_eq!(app.sim.reservoir, 15_000 - 600 - 52);
}

#[test]
fn a_slow_zero_cancels_the_extended_bolus_with_an_abort_record() {
    let mut app = App::connected();
    start(&mut app, bolus(600, 60, 0, 2));
    app.wait(15 * MINUTE);
    start(&mut app, bolus(0, 0, 0, 2));
    assert_eq!(app.events(), [(30, [150, 15, 0])]);
    assert_eq!(slow(&mut app), (0, vec![0; 7]));
    start(&mut app, bolus(0, 0, 0, 2));
    assert_eq!(app.events(), [(30, [150, 15, 0])]);
}

#[test]
fn a_combined_bolus_front_loads_its_immediate_part() {
    let mut app = App::connected();
    start(&mut app, bolus(600, 60, 100, 2));
    assert_eq!(app.events(), [(17, [600, 100, 60])]);
    app.wait(8_700);
    assert_eq!(slow(&mut app), (1, vec![1, 101, 600, 100, 100, 0, 60]));
    assert_eq!(immediate(&mut app), (0, vec![0, 0, 0]));
    app.wait(60 * MINUTE);
    assert_eq!(app.events(), [(18, [600, 100, 60])]);
    start(&mut app, bolus(600, 60, 100, 2));
    app.wait(10_000);
    start(&mut app, bolus(0, 0, 0, 2));
    assert_eq!(app.events()[1], (31, [101, 100, 0]));
}

#[test]
fn the_pump_refuses_an_extended_bolus_out_of_its_ranges() {
    let mut app = App::connected();
    for payload in [
        bolus(600, 14, 0, 2),
        bolus(600, 721, 0, 2),
        bolus(5, 60, 0, 2),
        bolus(600, 60, 5, 2),
        bolus(600, 60, 591, 2),
    ] {
        assert_eq!(
            app.send(Characteristic::StartStopBolus, &payload),
            BleResult::Status(0x82)
        );
    }
    start(&mut app, bolus(600, 60, 590, 2));
    assert_eq!(
        app.send(Characteristic::StartStopBolus, &bolus(600, 60, 0, 2)),
        BleResult::Status(0x87)
    );
    start(&mut app, bolus(100, 0, 0, 1));
}

#[test]
fn a_temp_basal_runs_reports_its_minutes_left_and_ends_in_place() {
    let mut app = App::connected();
    assert_eq!(temp(&mut app, 150, 30), BleResult::Done);
    assert_eq!(app.events(), [(9, [150, 30, 0])]);
    assert_eq!(fields(&app.system_status(), &[10, 14]), [150, 30]);
    app.wait(10 * MINUTE + 30_000);
    assert_eq!(fields(&app.system_status(), &[10, 14]), [150, 20]);
    app.wait(20 * MINUTE);
    assert_eq!(fields(&app.system_status(), &[10, 14]), [100, 0]);
    assert_eq!(app.events(), [(10, [150, 30, 0])]);
}

#[test]
fn a_hundred_percent_cancels_and_the_record_holds_the_minutes_it_ran() {
    let mut app = App::connected();
    assert_eq!(temp(&mut app, 100, 0), BleResult::Done);
    assert!(app.events().is_empty());
    assert_eq!(temp(&mut app, 0, 30), BleResult::Done);
    app.wait(10 * MINUTE);
    assert_eq!(temp(&mut app, 100, 0), BleResult::Done);
    assert_eq!(app.events(), [(10, [0, 10, 0])]);
    assert_eq!(fields(&app.system_status(), &[10]), [100]);
}

#[test]
fn the_pump_refuses_a_temp_basal_with_its_own_codes() {
    let mut app = App::connected();
    for (percent, minutes) in [(150, 14), (150, 241), (201, 30), (-1, 30)] {
        assert_eq!(temp(&mut app, percent, minutes), BleResult::Status(0x82));
    }
    let mut corrupt = tbr(150, 30);
    corrupt[15] ^= 1;
    for payload in [
        corrupt,
        tbr(150, 30)[..7].to_vec(),
        [tbr(150, 30), vec![0]].concat(),
    ] {
        assert_eq!(
            app.send(Characteristic::StartStopTbr, &payload),
            BleResult::Status(0x81)
        );
    }
    assert_eq!(temp(&mut app, 150, 30), BleResult::Done);
    assert_eq!(temp(&mut app, 120, 30), BleResult::Status(0x86));
}

#[test]
fn a_percent_above_the_long_scale_gets_the_short_duration_limit() {
    let mut app = App::connected();
    app.sim.settings.insert(setting::TBR_LONG_SCALE_MAX, 150);
    app.sim.settings.insert(setting::TBR_SHORT_SCALE_MAX, 250);
    app.sim.settings.insert(setting::TBR_SHORT_DURATION_MAX, 60);
    assert_eq!(temp(&mut app, 200, 90), BleResult::Status(0x82));
    assert_eq!(temp(&mut app, 251, 30), BleResult::Status(0x82));
    assert_eq!(temp(&mut app, 200, 60), BleResult::Done);
}

#[test]
fn basal_goes_in_every_three_minutes_at_the_active_profile_and_the_temp_percent() {
    let mut app = App::connected();
    app.wait(179_999);
    assert_eq!(fields(&app.system_status(), &[1]), [15_000]);
    app.wait(1);
    assert_eq!(fields(&app.system_status(), &[1]), [15_000 - 2]);
    app.wait(60 * MINUTE - 180_000);
    assert_eq!(app.sim.reservoir, 15_000 - 52);
    app.wait(1);
    assert_eq!(temp(&mut app, 200, 60), BleResult::Done);
    app.wait(60 * MINUTE);
    assert_eq!(app.sim.reservoir, 15_000 - 52 - 104);
    app.sim.settings.insert(setting::BASAL_PROFILE_SELECTION, 4);
    app.wait(60 * MINUTE);
    assert_eq!(app.sim.reservoir, 15_000 - 52 - 104 - 40);
    app.sim.stop(app.at());
    app.wait(60 * MINUTE);
    assert_eq!(app.sim.reservoir, 15_000 - 52 - 104 - 40);
}

#[test]
fn the_system_status_is_the_isc_layout_with_its_crc() {
    let mut app = App::connected();
    app.sim.battery = 6;
    app.sim.clock = 3_600;
    app.sim.settings.insert(setting::basal_a(1), 70);
    let status = app.system_status();
    assert_eq!(status.len(), 20);
    assert_eq!(
        status[..18],
        [10, 0x98, 0x3A, 0, 0, 6, 70, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(status[18..], crc16(&status[..18]).to_le_bytes());
    app.sim.settings.insert(setting::basal_a(1), -5);
    assert_eq!(fields(&app.system_status(), &[6]), [0]);
}

#[test]
fn stopping_the_pump_aborts_every_delivery_and_logs_the_mode() {
    let mut app = App::connected();
    start(&mut app, bolus(500, 0, 0, 1));
    start(&mut app, bolus(600, 60, 0, 2));
    assert_eq!(temp(&mut app, 150, 30), BleResult::Done);
    app.wait(10_000);
    app.sim.stop(app.at());
    assert_eq!(
        app.events(),
        [
            (29, [114, 0, 0]),
            (30, [1, 0, 0]),
            (32, [150, 0, 0]),
            (14, [3, 0, 0])
        ]
    );
    assert_eq!(app.system_status()[0], 3);
    assert_eq!(app.setting(setting::PUMP_MODE_SELECTION), 3);
    app.sim.stop(app.at());
    assert_eq!(app.events().len(), 4);
    app.sim.run(app.at());
    app.sim.run(app.at());
    assert_eq!(app.events()[4..], [(14, [10, 0, 0])]);
    assert_eq!(app.system_status()[0], 10);
}

#[test]
fn a_temp_basal_is_refused_with_0x87_while_delivery_is_not_allowed() {
    let mut app = App::connected();
    app.sim.stop(app.at());
    assert_eq!(temp(&mut app, 150, 30), BleResult::Status(0x87));
    assert_eq!(temp(&mut app, 100, 0), BleResult::Done);
    assert_eq!(app.events(), [(14, [3, 0, 0])]);
}

#[test]
fn an_alarm_is_logged_and_stops_the_pump() {
    let mut app = App::connected();
    app.now = 2_000;
    app.sim.alarm(Alarm::Occlusion, app.at());
    app.sim.alarm(Alarm::BatteryEmpty, app.at());
    let alarms: Vec<(u32, u8)> = app
        .sim
        .alarms
        .entries
        .iter()
        .map(|e| (e.timestamp, e.kind))
        .collect();
    assert_eq!(alarms, [(2, 105), (2, 101)]);
    assert_eq!(app.system_status()[0], 3);
    assert_eq!(
        app.get(Characteristic::AlarmEntryCount),
        Ok(super::safevar(2))
    );
}

#[test]
fn running_dry_raises_cartridge_empty_and_aborts_what_was_running() {
    let mut app = App::connected();
    app.sim.reservoir = 50;
    start(&mut app, bolus(100, 0, 0, 1));
    app.wait(100 * 87);
    assert_eq!(app.events(), [(29, [50, 0, 0]), (14, [3, 0, 0])]);
    assert_eq!(app.sim.alarms.entries[0].kind, 104);
    assert_eq!(app.sim.reservoir, 0);

    for (payload, aborted) in [
        (bolus(600, 15, 0, 2), (30, [2, 0, 0])),
        (bolus(600, 15, 100, 2), (31, [2, 2, 0])),
    ] {
        let mut app = App::connected();
        app.sim.reservoir = 2;
        start(&mut app, payload);
        app.wait(15 * MINUTE);
        assert_eq!(app.events()[0], aborted);
    }

    let mut app = App::connected();
    app.sim.reservoir = -1;
    app.wait(180_000);
    assert_eq!(app.sim.alarms.entries[0].kind, 104);
    assert_eq!(fields(&app.system_status(), &[1]), [-1]);
}

#[test]
fn an_unparseable_control_version_reads_as_the_oldest_format() {
    let mut app = App::connected();
    app.sim.versions.control = "x".into();
    assert_eq!(app.bolus_status().len(), 13);
    assert_eq!(app.system_status().len(), 18);
}
