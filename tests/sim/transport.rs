use pump_shared::BleResult;
use pump_ypso::protocol::{uuids, Characteristic, KEY_LEN};
use pump_ypso::{Frames, ProtoError};

use super::{connect, ids, read, safevar, write, App};

#[test]
fn a_setting_read_is_four_write_frames_one_read_and_three_extended_reads() {
    let mut app = App::connected();
    let writes = app.write_all(Characteristic::SettingId, &safevar(62));
    assert_eq!(writes, vec![BleResult::Done; 4]);
    let frames = app
        .read_frames(uuids::SETTINGS_SERVICE, uuids::SETTING_VALUE)
        .unwrap();
    assert_eq!(
        frames.iter().map(|f| f[0]).collect::<Vec<_>>(),
        vec![0x14, 0x24, 0x34, 0x44]
    );
    assert_eq!(
        app.op(read(uuids::CRYPTO_SERVICE, uuids::EXTENDED_READ)),
        BleResult::Status(0x07)
    );
}

#[test]
fn the_write_counter_must_be_strictly_above_the_last_accepted() {
    let mut app = App::connected();
    app.counter = 41;
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
    assert_eq!(app.sim.app_counter, 42);
    for counter in [41, 40] {
        app.counter = counter;
        assert_eq!(
            app.send(Characteristic::SettingId, &safevar(62)),
            BleResult::Status(0x8B)
        );
    }
    assert_eq!(app.sim.app_counter, 42);
    app.counter = 1000;
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn a_counter_with_the_sign_bit_set_is_always_refused() {
    let mut app = App::connected();
    for counter in [(1 << 63) - 1, u64::MAX - 1] {
        app.counter = counter;
        assert_eq!(
            app.send(Characteristic::SettingId, &safevar(62)),
            BleResult::Status(0x8B)
        );
    }
    assert_eq!(app.sim.app_counter, 0);
}

#[test]
fn another_app_moving_the_counter_ahead_makes_writes_0x8b_until_the_app_jumps_past() {
    let mut app = App::connected();
    app.sim.app_counter += 2000;
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Status(0x8B)
    );
    app.counter = 2000;
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn a_write_sealed_for_another_reboot_epoch_is_0x8b() {
    let mut app = App::connected();
    app.reboot = 1;
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Status(0x8B)
    );
    assert_eq!(app.sim.app_counter, 0);
}

#[test]
fn a_wrong_key_is_0x8a_on_writes_and_unreadable_replies() {
    let mut app = App::connected();
    app.sim.key = Some([0x43; KEY_LEN]);
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Status(0x8A)
    );
    assert_eq!(
        app.opened(Characteristic::GetSystemStatus),
        Err(ProtoError::Aead)
    );
    assert_eq!(app.sim.app_counter, 0);
}

#[test]
fn a_message_too_short_to_open_is_0x8a() {
    let mut app = App::connected();
    let (service, uuid) = ids(Characteristic::SettingId);
    let frame = [Frames::header(1, 1), 1, 2, 3];
    assert_eq!(
        app.op(write(service, uuid, &frame)),
        BleResult::Status(0x8A)
    );
}

#[test]
fn a_rejected_frame_resets_the_one_reassembly_buffer_with_0x8d() {
    let mut app = App::connected();
    let (service, uuid) = ids(Characteristic::SettingId);
    let (_, other) = ids(Characteristic::SettingValue);
    let first = [Frames::header(1, 4), 1, 2, 3];
    for (frame, context) in [
        (vec![Frames::header(2, 4), 1], uuid),
        (vec![Frames::header(1, 1)], uuid),
        (vec![Frames::header(1, 0), 1], uuid),
        (vec![Frames::header(0, 4), 1], uuid),
        (vec![Frames::header(1, 1); 288], uuid),
    ] {
        assert_eq!(
            app.op(write(service, context, &frame)),
            BleResult::Status(0x8D),
            "{frame:?}"
        );
    }
    for (frame, context) in [
        (vec![Frames::header(1, 4), 1], uuid),
        (vec![Frames::header(2, 4), 1], other),
        (vec![Frames::header(2, 5), 1], uuid),
        (vec![Frames::header(3, 4), 1], uuid),
        (vec![Frames::header(1, 4), 1], uuid),
        (vec![Frames::header(2, 4); 287], uuid),
    ] {
        assert_eq!(app.op(write(service, uuid, &first)), BleResult::Done);
        assert_eq!(
            app.op(write(service, context, &frame)),
            BleResult::Status(0x8D),
            "{frame:?}"
        );
    }
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn an_empty_write_is_refused_by_the_att_layer() {
    let mut app = App::connected();
    let (service, uuid) = ids(Characteristic::SettingId);
    assert_eq!(app.op(write(service, uuid, &[])), BleResult::Status(0x0D));
}

#[test]
fn replies_carry_the_pump_epoch_and_a_strictly_increasing_counter() {
    let mut app = App::connected();
    app.sim.reboot = 9;
    app.sim.pump_counter = 556;
    let first = app.opened(Characteristic::GetSystemStatus).unwrap();
    let second = app.opened(Characteristic::AlarmEntryCount).unwrap();
    assert_eq!((first.reboot, first.counter), (9, 557));
    assert_eq!((second.reboot, second.counter), (9, 558));
    assert_eq!(second.payload, safevar(0));
}

#[test]
fn replies_are_reproducible() {
    let frames = || {
        App::connected()
            .read_frames(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)
            .unwrap()
    };
    assert_eq!(frames(), frames());
}

#[test]
fn a_new_first_read_discards_the_continuations_still_owed() {
    let mut app = App::connected();
    app.op(read(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS));
    let opened = app.opened(Characteristic::SystemTime).unwrap();
    assert_eq!(opened.counter, 2);
}

#[test]
fn a_reboot_moves_the_epoch_drops_the_link_and_the_counting_carries_on() {
    let mut app = App::connected();
    app.sim.reboot = 9;
    app.reboot = 9;
    app.sim.pump_counter = 556;
    app.counter = 2044;
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
    app.sim.reboot(app.at());
    assert!(!app.sim.is_connected());
    assert_eq!(
        app.op(read(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)),
        BleResult::Disconnected
    );
    app.wait(45_000);
    assert_eq!(app.op(connect()), BleResult::Done);
    assert_eq!(app.authorise(&super::PASSWORD), BleResult::Done);
    let opened = app.opened(Characteristic::GetSystemStatus).unwrap();
    assert_eq!((opened.reboot, opened.counter), (10, 557));
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Status(0x8B)
    );
    app.reboot = 10;
    assert_eq!(
        app.send(Characteristic::SettingId, &safevar(62)),
        BleResult::Done
    );
    assert_eq!(app.sim.app_counter, 2047);
}
