#![cfg(feature = "sim")]

use pump_shared::{Address, BleOp, BleResult, NotifyKind, Timestamp, Uuid};
use pump_testkit::SimDevice;
use pump_ypso::protocol::{setting, uuids, KEY_LEN, NONCE_LEN};
use pump_ypso::sim::{Versions, YpsoSim, PASSWORD_LEN};
use pump_ypso::{protocol::assemble_serial, Crypto, Frames, Opened, ProtoError, Reassembler};

const KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
const PASSWORD: [u8; PASSWORD_LEN] = [0x5A; PASSWORD_LEN];
const SERIAL_LOWER: u16 = setting::SERIAL_NUMBER_LOWER;
const SERIAL_UPPER: u16 = setting::SERIAL_NUMBER_UPPER;
const JAN_1_2022_01_09_03: u64 = 8036 * 86_400 + 3600 + 9 * 60 + 3;

fn connect() -> BleOp {
    BleOp::Connect {
        address: Address::new([0xEC, 0x2A, 0xF0, 0x01, 0x02, 0x03]),
    }
}

fn read(service: Uuid, characteristic: Uuid) -> BleOp {
    BleOp::Read {
        service,
        characteristic,
    }
}

fn write(service: Uuid, characteristic: Uuid, data: &[u8]) -> BleOp {
    BleOp::Write {
        service,
        characteristic,
        data: data.to_vec(),
    }
}

fn safevar(value: i32) -> Vec<u8> {
    [value.to_le_bytes(), (!value).to_le_bytes()].concat()
}

struct App {
    sim: YpsoSim,
    crypto: Crypto,
    reboot: u32,
    counter: u64,
    now: Timestamp,
}

impl App {
    fn new(sim: YpsoSim) -> Self {
        let mut app = Self {
            sim,
            crypto: Crypto::new(KEY),
            reboot: 0,
            counter: 0,
            now: Timestamp::from_millis(0),
        };
        assert_eq!(app.op(connect()), BleResult::Done);
        app
    }

    fn connected() -> Self {
        Self::new(YpsoSim::new(KEY, PASSWORD))
    }

    fn op(&mut self, op: BleOp) -> BleResult {
        self.sim.handle(&op, self.now)
    }

    fn write_sealed(&mut self, characteristic: Uuid, payload: &[u8]) -> Vec<BleResult> {
        self.counter += 1;
        let sealed = self
            .crypto
            .seal(payload, self.reboot, self.counter, &[7; NONCE_LEN])
            .unwrap();
        Frames::split(&sealed)
            .unwrap()
            .into_iter()
            .map(|frame| self.op(write(uuids::SETTINGS_SERVICE, characteristic, &frame)))
            .collect()
    }

    fn last_status(&mut self, characteristic: Uuid, payload: &[u8]) -> BleResult {
        let results = self.write_sealed(characteristic, payload);
        let (last, rest) = results.split_last().unwrap();
        assert!(rest.iter().all(|r| *r == BleResult::Done), "{results:?}");
        last.clone()
    }

    fn read_frames(&mut self, service: Uuid, characteristic: Uuid) -> Vec<Vec<u8>> {
        let BleResult::Data(first) = self.op(read(service, characteristic)) else {
            panic!("first frame missing");
        };
        let (_, total) = Frames::parse_header(first[0]);
        let mut frames = vec![first];
        for _ in 1..total {
            let BleResult::Data(frame) = self.op(read(uuids::CRYPTO_SERVICE, uuids::EXTENDED_READ))
            else {
                panic!("continuation missing");
            };
            frames.push(frame);
        }
        frames
    }

    fn read_sealed(&mut self, service: Uuid, characteristic: Uuid) -> Result<Opened, ProtoError> {
        let mut reassembler = Reassembler::default();
        let mut message = None;
        for frame in self.read_frames(service, characteristic) {
            message = reassembler.push(&frame).unwrap();
        }
        self.crypto.open(&message.unwrap())
    }

    fn read_setting(&mut self, id: u16) -> i32 {
        assert_eq!(
            self.last_status(uuids::SETTING_ID, &safevar(i32::from(id))),
            BleResult::Done
        );
        let value = self
            .read_sealed(uuids::SETTINGS_SERVICE, uuids::SETTING_VALUE)
            .unwrap()
            .payload;
        assert_eq!(
            value[4..],
            (!i32::from_le_bytes(value[..4].try_into().unwrap())).to_le_bytes()
        );
        i32::from_le_bytes(value[..4].try_into().unwrap())
    }
}

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
    assert_eq!(sim.handle(&BleOp::Disconnect, now), BleResult::Done);
    assert_eq!(sim.handle(&version, now), BleResult::Disconnected);
}

#[test]
fn version_reads_are_clear_text_unframed_strings_of_the_validated_pump() {
    let mut app = App::connected();
    assert!(app.sim.has_crypto_service());
    for (service, characteristic, text) in [
        (
            uuids::PUMP_BASE_SERVICE,
            uuids::PUMP_BASE_SERVICE_VERSION,
            "1.2",
        ),
        (
            uuids::PUMP_BASE_SERVICE,
            uuids::MASTER_SOFTWARE_VERSION,
            "V05.02.03",
        ),
        (
            uuids::SETTINGS_SERVICE,
            uuids::SETTINGS_SERVICE_VERSION,
            "1.7",
        ),
        (
            uuids::HISTORY_SERVICE,
            uuids::HISTORY_SERVICE_VERSION,
            "1.4",
        ),
        (
            uuids::CONTROL_SERVICE,
            uuids::CONTROL_SERVICE_VERSION,
            "1.4",
        ),
    ] {
        assert_eq!(
            app.op(read(service, characteristic)),
            BleResult::Data(text.as_bytes().to_vec())
        );
    }
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
    assert_eq!(
        app.op(read(
            uuids::PUMP_BASE_SERVICE,
            uuids::MASTER_SOFTWARE_VERSION
        )),
        BleResult::Data(b"V35.00.01".to_vec())
    );
}

#[test]
fn characteristics_the_sim_does_not_serve_fail_at_the_link() {
    let mut app = App::connected();
    for op in [
        read(uuids::PUMP_BASE_SERVICE, uuids::SUPERVISOR_SOFTWARE_VERSION),
        read(uuids::SETTINGS_SERVICE, uuids::PUMP_BASE_SERVICE_VERSION),
        read(uuids::CONTROL_SERVICE, uuids::GET_BOLUS_STATUS),
        write(uuids::CONTROL_SERVICE, uuids::START_STOP_BOLUS, &[0x11]),
        write(
            uuids::SETTINGS_SERVICE,
            uuids::AUTHORIZATION_PASSWORD,
            &PASSWORD,
        ),
    ] {
        assert_eq!(app.op(op.clone()), BleResult::Failed, "{op:?}");
    }
}

#[test]
fn the_password_is_written_in_clear_and_a_wrong_one_is_refused_with_0x08() {
    let mut app = App::connected();
    let password = |data: &[u8]| {
        write(
            uuids::PUMP_BASE_SERVICE,
            uuids::AUTHORIZATION_PASSWORD,
            data,
        )
    };
    assert_eq!(
        app.op(password(&[0; PASSWORD_LEN])),
        BleResult::Status(0x08)
    );
    assert_eq!(app.op(password(&PASSWORD[..15])), BleResult::Status(0x08));
    assert_eq!(app.op(password(&PASSWORD)), BleResult::Done);
}

#[test]
fn a_setting_read_is_four_write_frames_one_read_and_three_extended_reads() {
    let mut app = App::connected();
    let writes = app.write_sealed(uuids::SETTING_ID, &safevar(i32::from(SERIAL_LOWER)));
    assert_eq!(writes, vec![BleResult::Done; 4]);
    let frames = app.read_frames(uuids::SETTINGS_SERVICE, uuids::SETTING_VALUE);
    assert_eq!(
        frames.iter().map(|f| f[0]).collect::<Vec<_>>(),
        vec![0x14, 0x24, 0x34, 0x44]
    );
    assert_eq!(
        app.op(read(uuids::CRYPTO_SERVICE, uuids::EXTENDED_READ)),
        BleResult::Failed
    );
}

#[test]
fn the_serial_is_served_lower_then_upper_as_the_handover_example() {
    let mut app = App::connected();
    let lower = app.read_setting(SERIAL_LOWER);
    let upper = app.read_setting(SERIAL_UPPER);
    assert_eq!((lower, upper), (63_011, 154));
    assert_eq!(assemble_serial(upper as u16, lower as u16), 10_155_555);
}

#[test]
fn settings_are_a_scenario_knob() {
    let mut app = App::connected();
    app.sim.settings.insert(setting::PUMP_MODE_SELECTION, 10);
    assert_eq!(app.read_setting(setting::PUMP_MODE_SELECTION), 10);
}

#[test]
fn a_bad_setting_selection_is_answered_with_the_spec_codes() {
    let mut app = App::connected();
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(999)),
        BleResult::Status(0x83)
    );
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(-1)),
        BleResult::Status(0x83)
    );
    let mut corrupt = safevar(i32::from(SERIAL_LOWER));
    corrupt[7] ^= 1;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &corrupt),
        BleResult::Status(0x81)
    );
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &[62, 0]),
        BleResult::Status(0x81)
    );
}

#[test]
fn a_setting_value_with_nothing_selected_is_not_simulated() {
    let mut app = App::connected();
    assert_eq!(
        app.op(read(uuids::SETTINGS_SERVICE, uuids::SETTING_VALUE)),
        BleResult::Failed
    );
}

#[test]
fn the_write_counter_must_be_strictly_above_the_last_accepted() {
    let mut app = App::connected();
    app.counter = 41;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Done
    );
    assert_eq!(app.sim.app_counter, 42);
    app.counter = 41;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8B)
    );
    app.counter = 40;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8B)
    );
    assert_eq!(app.sim.app_counter, 42);
    app.counter = 1000;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn a_counter_with_the_sign_bit_set_is_always_refused() {
    let mut app = App::connected();
    app.counter = (1 << 63) - 1;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8B)
    );
    app.counter = u64::MAX - 1;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8B)
    );
    assert_eq!(app.sim.app_counter, 0);
}

#[test]
fn another_app_moving_the_counter_ahead_makes_writes_0x8b_until_the_app_jumps_past() {
    let mut app = App::connected();
    app.sim.app_counter += 2000;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8B)
    );
    app.counter = 2000;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn a_write_sealed_for_another_reboot_epoch_is_0x8b() {
    let mut app = App::connected();
    app.reboot = 1;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8B)
    );
    assert_eq!(app.sim.app_counter, 0);
}

#[test]
fn a_wrong_key_is_0x8a_on_writes_and_unreadable_replies() {
    let mut app = App::connected();
    app.sim.key = [0x43; KEY_LEN];
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8A)
    );
    assert_eq!(
        app.read_sealed(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS),
        Err(ProtoError::Aead)
    );
    assert_eq!(app.sim.app_counter, 0);
}

#[test]
fn a_message_too_short_to_open_is_0x8a() {
    let mut app = App::connected();
    let frame = [Frames::header(1, 1), 1, 2, 3];
    assert_eq!(
        app.op(write(uuids::SETTINGS_SERVICE, uuids::SETTING_ID, &frame)),
        BleResult::Status(0x8A)
    );
}

#[test]
fn malformed_framing_fails_and_does_not_poison_the_next_write() {
    let mut app = App::connected();
    let frame = [Frames::header(2, 4), 1, 2, 3];
    assert_eq!(
        app.op(write(uuids::SETTINGS_SERVICE, uuids::SETTING_ID, &frame)),
        BleResult::Failed
    );
    let first = [Frames::header(1, 4), 1, 2, 3];
    let op = write(uuids::SETTINGS_SERVICE, uuids::SETTING_ID, &first);
    assert_eq!(app.op(op.clone()), BleResult::Done);
    assert_eq!(app.op(op), BleResult::Failed);
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn system_status_is_the_fourteen_byte_layout_in_four_frames() {
    let mut app = App::connected();
    app.sim.delivery_state = 3;
    app.sim.reservoir = 15_000;
    app.sim.battery = 4;
    app.sim.basal_rate = 52;
    app.sim.tbr_percent = 192;
    let frames = app.read_frames(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS);
    assert_eq!(frames.len(), 4);
    assert_eq!(frames.concat().len() - 4, 66);
    let opened = app
        .read_sealed(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)
        .unwrap();
    assert_eq!(
        opened.payload,
        [3, 0x98, 0x3A, 0, 0, 4, 52, 0, 0, 0, 192, 0, 0, 0]
    );
}

#[test]
fn replies_carry_the_pump_epoch_and_a_strictly_increasing_counter() {
    let mut app = App::connected();
    app.sim.reboot = 9;
    app.sim.pump_counter = 556;
    let first = app
        .read_sealed(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)
        .unwrap();
    let second = app
        .read_sealed(uuids::HISTORY_SERVICE, uuids::ALARM_ENTRY_COUNT)
        .unwrap();
    assert_eq!((first.reboot, first.counter), (9, 557));
    assert_eq!((second.reboot, second.counter), (9, 558));
    assert_eq!(second.payload, safevar(0));
}

#[test]
fn replies_are_reproducible() {
    let frames = || App::connected().read_frames(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS);
    assert_eq!(frames(), frames());
}

#[test]
fn a_new_first_read_discards_the_continuations_still_owed() {
    let mut app = App::connected();
    app.op(read(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS));
    let opened = app
        .read_sealed(uuids::SETTINGS_SERVICE, uuids::SYSTEM_TIME)
        .unwrap();
    assert_eq!(opened.counter, 2);
}

#[test]
fn date_and_time_match_the_hardware_example() {
    let mut app = App::connected();
    app.sim.clock = JAN_1_2022_01_09_03;
    let date = app.read_frames(uuids::SETTINGS_SERVICE, uuids::SYSTEM_DATE);
    let time = app.read_frames(uuids::SETTINGS_SERVICE, uuids::SYSTEM_TIME);
    assert_eq!((date.len(), time.len()), (3, 3));
    let date = app
        .read_sealed(uuids::SETTINGS_SERVICE, uuids::SYSTEM_DATE)
        .unwrap();
    let time = app
        .read_sealed(uuids::SETTINGS_SERVICE, uuids::SYSTEM_TIME)
        .unwrap();
    assert_eq!(date.payload, [0xE6, 0x07, 0x01, 0x01]);
    assert_eq!(time.payload, [0x01, 0x09, 0x03]);
}

#[test]
fn the_pump_clock_runs_on_virtual_time() {
    let mut app = App::connected();
    app.sim.clock = JAN_1_2022_01_09_03;
    app.now = Timestamp::from_millis((59 * 86_400 + 22 * 3600 + 50 * 60 + 57) * 1000 + 999);
    let date = app
        .read_sealed(uuids::SETTINGS_SERVICE, uuids::SYSTEM_DATE)
        .unwrap();
    let time = app
        .read_sealed(uuids::SETTINGS_SERVICE, uuids::SYSTEM_TIME)
        .unwrap();
    assert_eq!(date.payload, [0xE6, 0x07, 0x03, 0x02]);
    assert_eq!(time.payload, [0, 0, 0]);
    app.sim.clock = 0;
    app.now = Timestamp::from_millis(8825 * 86_400 * 1000);
    let date = app
        .read_sealed(uuids::SETTINGS_SERVICE, uuids::SYSTEM_DATE)
        .unwrap();
    assert_eq!(date.payload, [0xE8, 0x07, 0x02, 0x1D]);
}

#[test]
fn a_reboot_moves_the_epoch_drops_the_link_and_the_counting_carries_on() {
    let mut app = App::connected();
    app.sim.reboot = 9;
    app.reboot = 9;
    app.sim.pump_counter = 556;
    app.counter = 2044;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Done
    );
    app.sim.reboot();
    assert!(!app.sim.is_connected());
    assert_eq!(
        app.op(read(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)),
        BleResult::Disconnected
    );
    assert_eq!(app.op(connect()), BleResult::Done);
    let opened = app
        .read_sealed(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)
        .unwrap();
    assert_eq!((opened.reboot, opened.counter), (10, 557));
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Status(0x8B)
    );
    app.reboot = 10;
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Done
    );
    assert_eq!(app.sim.app_counter, 2047);
}

#[test]
fn a_reconnect_drops_a_half_received_write() {
    let mut app = App::connected();
    let first = [Frames::header(1, 4), 1, 2, 3];
    assert_eq!(
        app.op(write(uuids::SETTINGS_SERVICE, uuids::SETTING_ID, &first)),
        BleResult::Done
    );
    assert_eq!(app.op(connect()), BleResult::Done);
    assert_eq!(
        app.last_status(uuids::SETTING_ID, &safevar(62)),
        BleResult::Done
    );
}

#[test]
fn the_sim_is_debuggable() {
    let sim = YpsoSim::new(KEY, PASSWORD);
    assert!(format!("{sim:?}").starts_with("YpsoSim"));
    assert_eq!(sim.versions.clone(), sim.versions);
}

#[cfg(feature = "opn")]
mod opn {
    use super::*;

    #[test]
    fn an_opn_pump_has_no_crypto_service_and_answers_in_clear() {
        let mut app = App::new(YpsoSim::opn(PASSWORD));
        assert!(!app.sim.has_crypto_service());
        assert_eq!(
            app.op(read(uuids::CONTROL_SERVICE, uuids::CONTROL_SERVICE_VERSION)),
            BleResult::Data(b"1.0".to_vec())
        );
        assert_eq!(
            app.op(read(
                uuids::PUMP_BASE_SERVICE,
                uuids::MASTER_SOFTWARE_VERSION
            )),
            BleResult::Data(b"V65.00.06".to_vec())
        );
        assert_eq!(
            app.op(read(uuids::CRYPTO_SERVICE, uuids::EXTENDED_READ)),
            BleResult::Failed
        );
        assert_eq!(
            app.op(write(
                uuids::PUMP_BASE_SERVICE,
                uuids::AUTHORIZATION_PASSWORD,
                &PASSWORD
            )),
            BleResult::Done
        );
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
        assert_eq!(
            app.op(write(
                uuids::SETTINGS_SERVICE,
                uuids::SETTING_ID,
                &safevar(7)
            )),
            BleResult::Status(0x83)
        );
        assert_eq!(
            app.op(read(uuids::CONTROL_SERVICE, uuids::GET_SYSTEM_STATUS)),
            BleResult::Data(vec![10, 0x98, 0x3A, 0, 0, 4, 52, 0, 0, 0, 100, 0, 0, 0])
        );
        assert_eq!(app.sim.pump_counter, 0);
    }
}
