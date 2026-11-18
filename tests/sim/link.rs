use pump_shared::{BleOp, BleResult, KeyRef, NotifyKind, Timestamp};
use pump_testkit::SimDevice;
use pump_ypso::protocol::{uuids, Characteristic};
use pump_ypso::sim::{Versions, YpsoSim};

use super::{connect, ids, read, write, App, KEY, PASSWORD};

#[test]
fn the_link_answers_like_a_real_link_and_refuses_ops_when_down() {
    let mut sim = YpsoSim::new(KEY, PASSWORD);
    let now = Timestamp::from_millis(0);
    let version = read(uuids::PUMP_BASE_SERVICE, uuids::PUMP_BASE_SERVICE_VERSION);
    for op in [
        version.clone(),
        BleOp::Disconnect,
        BleOp::RequestFastInterval,
        write(
            uuids::PUMP_BASE_SERVICE,
            uuids::AUTHORIZATION_PASSWORD,
            &PASSWORD,
        ),
    ] {
        assert_eq!(sim.handle(&op, now), BleResult::Disconnected, "{op:?}");
    }
    assert!(!sim.is_connected());
    assert_eq!(sim.handle(&connect(), now), BleResult::Done);
    assert!(sim.is_connected());
    for op in [
        BleOp::RequestFastInterval,
        BleOp::Subscribe {
            service: uuids::CONTROL_SERVICE,
            characteristic: uuids::CONTROL_NOTIFY,
            kind: NotifyKind::Notification,
        },
        BleOp::Unsubscribe {
            service: uuids::CONTROL_SERVICE,
            characteristic: uuids::CONTROL_NOTIFY,
        },
    ] {
        assert_eq!(sim.handle(&op, now), BleResult::Done);
    }
    assert_eq!(sim.poll_notification(now), None);
    assert_eq!(sim.handle(&BleOp::Disconnect, now), BleResult::Done);
    assert_eq!(sim.handle(&version, now), BleResult::Disconnected);
}

#[test]
fn version_reads_are_clear_text_unframed_and_need_no_authorisation() {
    let mut app = App::new(YpsoSim::new(KEY, PASSWORD));
    assert!(app.sim.has_crypto_service());
    for (characteristic, text) in [
        (Characteristic::PumpBaseServiceVersion, "1.2"),
        (Characteristic::MasterSoftwareVersion, "V05.02.03"),
        (Characteristic::SettingsServiceVersion, "1.7"),
        (Characteristic::HistoryServiceVersion, "1.4"),
        (Characteristic::ControlServiceVersion, "1.4"),
    ] {
        let (service, uuid) = ids(characteristic);
        assert_eq!(
            app.op(read(service, uuid)),
            BleResult::Data(text.as_bytes().to_vec())
        );
    }
}

#[test]
fn the_supervisor_version_is_a_ten_byte_runtime_value() {
    let mut app = App::new(YpsoSim::new(KEY, PASSWORD));
    let (service, uuid) = ids(Characteristic::SupervisorSoftwareVersion);
    assert_eq!(
        app.op(read(service, uuid)),
        BleResult::Data(b"V05.02.03\0".to_vec())
    );
    app.sim.supervisor = *b"0123456789";
    assert_eq!(
        app.op(read(service, uuid)),
        BleResult::Data(b"0123456789".to_vec())
    );
}

#[test]
fn version_strings_are_scenario_knobs() {
    let mut app = App::connected();
    app.sim.versions = Versions {
        pump_base: "1.1".into(),
        master_software: "V35.00.01".into(),
        settings: "1.3".into(),
        history: "1.1".into(),
        control: "1.0".into(),
    };
    let (service, uuid) = ids(Characteristic::MasterSoftwareVersion);
    assert_eq!(
        app.op(read(service, uuid)),
        BleResult::Data(b"V35.00.01".to_vec())
    );
    assert_eq!(app.sim.versions.clone(), app.sim.versions);
}

#[test]
fn characteristics_the_sources_do_not_define_fail_at_the_link() {
    let mut app = App::connected();
    for op in [
        read(uuids::PUMP_BASE_SERVICE, uuids::PAIRING_PASSKEY),
        read(uuids::SETTINGS_SERVICE, uuids::PUMP_BASE_SERVICE_VERSION),
        read(uuids::CONTROL_SERVICE, uuids::START_STOP_BOLUS),
        read(uuids::CONTROL_SERVICE, uuids::CONTROL_NOTIFY),
        read(uuids::CRYPTO_SERVICE, uuids::CRYPTO_SERVICE_VERSION),
        read(uuids::SETTINGS_SERVICE, uuids::SETTING_ID),
        write(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS, &[0x11]),
        write(
            uuids::CONTROL_SERVICE,
            uuids::CONTROL_SERVICE_VERSION,
            &[0x11],
        ),
        write(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY, &[0x11]),
        write(
            uuids::SETTINGS_SERVICE,
            uuids::AUTHORIZATION_PASSWORD,
            &PASSWORD,
        ),
        write(uuids::SETTINGS_SERVICE, uuids::XXX_WRITE, &[0x11]),
    ] {
        assert_eq!(app.op(op.clone()), BleResult::Failed, "{op:?}");
    }
}

#[test]
fn a_reconnect_drops_a_half_received_write_and_the_authorisation() {
    let mut app = App::connected();
    assert!(app.sim.is_authorised());
    let (service, uuid) = ids(Characteristic::SettingId);
    let first = [pump_ypso::Frames::header(1, 4), 1, 2, 3];
    assert_eq!(app.op(write(service, uuid, &first)), BleResult::Done);
    assert_eq!(app.op(connect()), BleResult::Done);
    assert!(!app.sim.is_authorised());
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert_eq!(
        app.send(Characteristic::SettingId, &super::safevar(62)),
        BleResult::Done
    );
}

#[test]
fn the_host_key_store_agrees_with_the_server_secret() {
    let mut sim = YpsoSim::new(KEY, PASSWORD);
    let peer = sim.pump_public();
    let shared = sim.key_agree(&KeyRef::new("server"), &peer).unwrap();
    let expected = curve25519_dalek::montgomery::MontgomeryPoint(peer)
        .mul_clamped(sim.server_secret)
        .to_bytes();
    assert_eq!(shared, expected);
}

#[test]
fn the_sim_is_debuggable() {
    let sim = YpsoSim::new(KEY, PASSWORD);
    assert!(format!("{sim:?}").starts_with("YpsoSim"));
}
