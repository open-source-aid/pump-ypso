use pump_shared::{BleResult, KeyRef};
use pump_ypso::protocol::{setting, uuids, Characteristic};
use pump_ypso::sim::YpsoSim;
use tester_harness::SimDevice;

use super::{ids, le_i32, read, safevar, tbr, write, App, PASSWORD};

fn opn() -> App {
    App::authorised(YpsoSim::opn(PASSWORD))
}

#[test]
fn an_opn_pump_has_no_crypto_service_and_answers_in_clear() {
    let mut app = App::new(YpsoSim::opn(PASSWORD));
    assert!(!app.sim.has_crypto_service());
    for (characteristic, text) in [
        (Characteristic::PumpBaseServiceVersion, "1.1"),
        (Characteristic::MasterSoftwareVersion, "V65.00.06"),
        (Characteristic::SettingsServiceVersion, "1.3"),
        (Characteristic::HistoryServiceVersion, "1.1"),
        (Characteristic::ControlServiceVersion, "1.0"),
    ] {
        let (service, uuid) = ids(characteristic);
        assert_eq!(
            app.op(read(service, uuid)),
            BleResult::Data(text.as_bytes().to_vec())
        );
    }
    assert_eq!(
        app.get(Characteristic::GetSystemStatus),
        Err(BleResult::Status(0x08))
    );
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert_eq!(
        app.op(write(
            uuids::SETTINGS_SERVICE,
            uuids::SETTING_ID,
            &safevar(62)
        )),
        BleResult::Done
    );
    assert_eq!(
        app.op(read(uuids::SETTINGS_SERVICE, uuids::SETTING_VALUE)),
        BleResult::Data(safevar(63_011))
    );
    assert_eq!(app.sim.pump_counter, 0);
    assert_eq!(app.sim.app_counter, 0);
}

#[test]
fn an_opn_pump_has_no_crypto_characteristics_and_no_key_to_agree() {
    let mut app = opn();
    for op in [
        read(uuids::CRYPTO_SERVICE, uuids::EXTENDED_READ),
        read(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY),
        write(
            uuids::CRYPTO_SERVICE,
            uuids::WRITE_CHALLENGE_UTC_APP_PUB_KEY,
            &[0x11, 1],
        ),
    ] {
        assert_eq!(app.op(op.clone()), BleResult::Failed, "{op:?}");
    }
    assert_eq!(app.sim.key_agree(&KeyRef::new("server"), &[9; 32]), None);
}

#[test]
fn the_opn_system_status_is_eighteen_bytes_without_a_crc() {
    let mut app = opn();
    assert_eq!(
        app.system_status(),
        [10, 0x98, 0x3A, 0, 0, 4, 52, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn an_opn_bolus_is_one_safevar_and_its_status_thirteen_clear_bytes() {
    let mut app = opn();
    assert_eq!(
        app.send(Characteristic::StartStopBolus, &safevar(500)),
        BleResult::Done
    );
    app.wait(139 * 87);
    let status = app.bolus_status();
    assert_eq!(status.len(), 13);
    assert_eq!(
        (
            status[0],
            le_i32(&status, 1),
            le_i32(&status, 5),
            le_i32(&status, 9)
        ),
        (1, 139, 500, 1)
    );
    assert_eq!(
        app.send(Characteristic::StartStopBolus, &safevar(0)),
        BleResult::Done
    );
    assert_eq!(app.bolus_status()[0], 4);
    assert_eq!(app.events(), [(2, [139, 0, 0])]);
    let mut corrupt = safevar(100);
    corrupt[4] ^= 1;
    for payload in [corrupt, super::bolus(100, 30, 0, 2)] {
        assert_eq!(
            app.send(Characteristic::StartStopBolus, &payload),
            BleResult::Status(0x81)
        );
    }
    assert_eq!(
        app.send(Characteristic::StartStopBolus, &safevar(9)),
        BleResult::Status(0x82)
    );
}

#[test]
fn an_opn_temp_basal_and_history_use_the_same_commands_in_clear() {
    let mut app = opn();
    assert_eq!(
        app.send(Characteristic::StartStopTbr, &tbr(150, 30)),
        BleResult::Done
    );
    assert_eq!(le_i32(&app.system_status(), 10), 150);
    assert_eq!(app.get(Characteristic::EventEntryCount), Ok(safevar(1)));
    assert_eq!(
        app.send(Characteristic::EventEntryIndex, &[0, 0]),
        BleResult::Done
    );
    let entry = app.get(Characteristic::EventEntryValue).unwrap();
    assert_eq!((entry.len(), entry[4]), (19, 9));
    assert_eq!(
        app.send(Characteristic::EventEntryIndex, &safevar(0)),
        BleResult::Status(0x81)
    );
    assert_eq!(app.setting(setting::SERIAL_NUMBER_UPPER), 154);
}

#[test]
fn an_opn_pump_has_no_crypto_pause_after_a_reboot() {
    let mut app = opn();
    app.sim.reboot(app.at());
    assert_eq!(app.op(super::connect()), BleResult::Done);
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert!(app.get(Characteristic::GetSystemStatus).is_ok());
}
