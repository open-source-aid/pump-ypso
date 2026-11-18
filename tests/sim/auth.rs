use pump_shared::BleResult;
use pump_ypso::protocol::{uuids, Characteristic};
use pump_ypso::sim::{YpsoSim, PASSWORD_LEN};

use super::{ids, read, safevar, write, App, KEY, PASSWORD};

#[test]
fn a_password_write_always_succeeds_and_only_a_matching_one_grants_access() {
    let mut app = App::new(YpsoSim::new(KEY, PASSWORD));
    assert_eq!(app.authorise(&[0; PASSWORD_LEN]), BleResult::Done);
    assert_eq!(app.authorise(&PASSWORD[..15]), BleResult::Done);
    assert!(!app.sim.is_authorised());
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert!(app.sim.is_authorised());
    assert_eq!(app.authorise(&[0; PASSWORD_LEN]), BleResult::Done);
    assert!(app.sim.is_authorised());
}

#[test]
fn before_the_password_the_probe_reads_and_commands_are_refused_with_0x08() {
    let mut app = App::new(YpsoSim::new(KEY, PASSWORD));
    for characteristic in [
        Characteristic::GetSystemStatus,
        Characteristic::AlarmEntryCount,
    ] {
        assert_eq!(app.get(characteristic), Err(BleResult::Status(0x08)));
    }
    assert_eq!(
        app.send(Characteristic::StartStopTbr, &super::tbr(150, 30)),
        BleResult::Status(0x08)
    );
    assert_eq!(app.sim.app_counter, 0);
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert!(app.get(Characteristic::GetSystemStatus).is_ok());
}

#[test]
fn before_the_password_a_setting_is_selected_but_its_value_is_0x08_or_0x83() {
    let mut app = App::new(YpsoSim::new(KEY, PASSWORD));
    assert_eq!(
        app.get(Characteristic::SettingValue),
        Err(BleResult::Status(0x83))
    );
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
    assert_eq!(
        app.get(Characteristic::SettingValue),
        Err(BleResult::Status(0x08))
    );
    assert_eq!(
        app.send(Characteristic::SettingValue, &safevar(1)),
        BleResult::Status(0x08)
    );
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(123)),
        BleResult::Done
    );
    assert_eq!(
        app.get(Characteristic::SettingValue),
        Err(BleResult::Status(0x83))
    );
}

#[test]
fn a_device_service_write_before_the_password_is_0x02() {
    let mut app = App::new(YpsoSim::new(KEY, PASSWORD));
    let (service, uuid) = ids(Characteristic::SupervisorSoftwareVersion);
    assert_eq!(app.op(write(service, uuid, &[1])), BleResult::Status(0x02));
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert_eq!(app.op(write(service, uuid, &[1])), BleResult::Failed);
}

#[test]
fn a_disconnect_ends_the_access_level() {
    let mut app = App::connected();
    assert_eq!(app.op(pump_shared::BleOp::Disconnect), BleResult::Done);
    assert!(!app.sim.is_authorised());
}

#[test]
fn a_link_that_is_not_encrypted_or_not_bonded_is_refused_at_the_att_layer() {
    let mut app = App::connected();
    let version = read(uuids::PUMP_BASE_SERVICE, uuids::PUMP_BASE_SERVICE_VERSION);
    for (encrypted, bonded, code) in [(false, true, 0x0F), (true, false, 0x05)] {
        app.sim.encrypted_link = encrypted;
        app.sim.bonded = bonded;
        assert_eq!(
            app.get(Characteristic::GetSystemStatus),
            Err(BleResult::Status(code))
        );
        assert_eq!(app.authorise(&PASSWORD), BleResult::Status(code));
        assert_eq!(
            app.write_all(Characteristic::SettingId, &safevar(62))[0],
            BleResult::Status(code)
        );
        assert_eq!(
            app.op(read(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY)),
            BleResult::Status(code)
        );
        assert!(matches!(app.op(version.clone()), BleResult::Data(_)));
    }
}

#[test]
fn after_a_reboot_the_pump_is_not_ready_for_45_seconds() {
    let mut app = App::connected();
    app.sim.reboot(app.at());
    app.reboot = 1;
    assert_eq!(app.op(super::connect()), BleResult::Done);
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert_eq!(
        app.get(Characteristic::GetSystemStatus),
        Err(BleResult::Status(0x89))
    );
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Status(0x89)
    );
    app.wait(44_999);
    assert_eq!(
        app.get(Characteristic::GetSystemStatus),
        Err(BleResult::Status(0x89))
    );
    app.wait(1);
    assert!(app.get(Characteristic::GetSystemStatus).is_ok());
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn a_pump_with_no_session_key_cannot_answer_a_sealed_read() {
    let mut app = App::connected();
    app.sim.key = None;
    assert_eq!(
        app.op(read(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)),
        BleResult::Status(0x8C)
    );
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Status(0x8A)
    );
    assert_eq!(app.sim.pump_counter, 0);
}
