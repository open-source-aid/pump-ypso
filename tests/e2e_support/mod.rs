#![allow(dead_code)]

pub mod scenarios;

use std::collections::VecDeque;
use std::time::Duration;

use pump_shared::{
    Address, Advertisement, BleOp, BleResult, Command, CommandId, Config, Driver, Engine, Event,
    Outcome, Slot, Timestamp,
};
use pump_testkit::{Harness, SimDevice};
use pump_ypso::protocol::{setting, uuids, Crypto, Frames, Reassembler, KEY_LEN, NONCE_LEN};
use pump_ypso::sim::{YpsoSim, PASSWORD_LEN};
use pump_ypso::DRIVER;

pub const KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
pub const PASSWORD: [u8; PASSWORD_LEN] = [0x5A; PASSWORD_LEN];
pub const SERIAL: u32 = 10_155_555;
pub const MAC: [u8; 6] = [0xEC, 0x2A, 0xF0, 0x01, 0x02, 0x03];
pub const NOON: u64 = 9497 * 86_400 + 12 * 3600 + 60;

pub trait Device: SimDevice {
    fn sim(&mut self) -> &mut YpsoSim;
}

impl Device for YpsoSim {
    fn sim(&mut self) -> &mut YpsoSim {
        self
    }
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    crc
}

pub fn crc16(value: &[u8]) -> [u8; 2] {
    let mut buf = value.to_vec();
    buf.resize(value.len().div_ceil(4) * 4, 0);
    for block in buf.chunks_exact_mut(4) {
        block.reverse();
        block.iter_mut().for_each(|b| *b = b.reverse_bits());
    }
    ((crc32_update(!0, &buf).reverse_bits() & 0xFFFF) as u16).to_le_bytes()
}

pub struct Blob {
    pub version: u8,
    pub flags: u8,
    pub write: i64,
    pub read: i64,
    pub reboot: u32,
    pub source: u8,
    pub valid_until: i64,
    pub prefix: [u8; 16],
    pub credentials: Vec<(Vec<u8>, [u8; PASSWORD_LEN])>,
}

impl Default for Blob {
    fn default() -> Self {
        Self {
            version: 1,
            flags: 0,
            write: -1,
            read: -1,
            reboot: 0,
            source: 0,
            valid_until: 0,
            prefix: [7; 16],
            credentials: vec![(b"O2".to_vec(), PASSWORD)],
        }
    }
}

impl Blob {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = b"YB".to_vec();
        out.extend([self.version, self.flags]);
        out.extend(MAC);
        out.extend(SERIAL.to_le_bytes());
        out.extend(KEY);
        out.extend(self.write.to_le_bytes());
        out.extend(self.read.to_le_bytes());
        out.extend(self.reboot.to_le_bytes());
        out.extend(self.valid_until.to_le_bytes());
        out.push(self.source);
        out.extend(self.prefix);
        out.push(self.credentials.len() as u8);
        for (code, password) in &self.credentials {
            out.push(code.len() as u8);
            out.extend(code);
            out.extend(password);
        }
        let crc = !crc32_update(!0, &out);
        out.extend(crc.to_le_bytes());
        out
    }
}

pub fn harness<D: Device>(device: D, blob: &Blob) -> Harness<D> {
    let mut harness = Harness::new(device);
    harness.announced = Some(DRIVER.info());
    harness.store.insert(Slot::Session, blob.encode());
    harness
}

pub fn engine() -> Box<dyn Engine> {
    let adv = Advertisement {
        address: Address::new(MAC),
        name: Some("YpsoPump_10155555"),
        service_uuids: &[],
        manufacturer: None,
        rssi: -50,
    };
    let variant = DRIVER.matches(&adv).expect("a YpsoPump advertisement");
    DRIVER.new_engine(variant, Config::default())
}

pub fn sim() -> YpsoSim {
    configure(YpsoSim::new(KEY, PASSWORD))
}

#[cfg(feature = "opn")]
pub fn opn() -> YpsoSim {
    configure(YpsoSim::opn(PASSWORD))
}

fn configure(mut sim: YpsoSim) -> YpsoSim {
    sim.clock = NOON;
    for (id, value) in [
        (setting::BASAL_PROFILE_SELECTION, 3),
        (setting::BOLUS_STEP_SELECTION, 5),
        (setting::BASAL_RATE_MIN, 2),
        (setting::BASAL_RATE_MAX, 2_500),
        (setting::TBR_LONG_SCALE_MAX, 200),
        (setting::TBR_LONG_DURATION_MAX, 240),
        (setting::TBR_SHORT_SCALE_MAX, 250),
        (setting::TBR_SHORT_DURATION_MAX, 120),
        (setting::TBR_END_NOT_INDICATED, 0),
    ] {
        sim.settings.insert(id, value);
    }
    for hour in 0..24 {
        sim.settings.insert(setting::basal_a(hour), 80);
    }
    sim
}

pub struct Run<D> {
    pub harness: Harness<D>,
    pub engine: Box<dyn Engine>,
}

impl<D: Device> Run<D> {
    pub fn new(device: D, blob: &Blob) -> Self {
        let mut run = Self {
            harness: harness(device, blob),
            engine: engine(),
        };
        run.settle();
        run
    }

    pub fn settle(&mut self) {
        self.harness
            .advance(self.engine.as_mut(), Duration::from_secs(120))
            .expect("no violation");
    }

    pub fn submit(&mut self, command: Command) -> CommandId {
        let id = self.engine.submit(command).expect("queued");
        self.settle();
        id
    }

    pub fn outcome(&self, id: CommandId) -> Option<Outcome> {
        self.harness.events.iter().find_map(|event| match event {
            Event::Finished { id: done, outcome } if *done == id => Some(outcome.clone()),
            _ => None,
        })
    }

    pub fn run(&mut self, command: Command) -> Outcome {
        let id = self.submit(command);
        self.outcome(id).expect("finished")
    }
}

pub fn entry(seconds: u32, kind: u8, values: [u16; 3], sequence: u32, index: u16) -> Vec<u8> {
    let mut bytes = seconds.to_le_bytes().to_vec();
    bytes.push(kind);
    values.iter().for_each(|v| bytes.extend(v.to_le_bytes()));
    bytes.extend(sequence.to_le_bytes());
    bytes.extend(index.to_le_bytes());
    let crc = crc16(&bytes);
    bytes.extend(crc);
    bytes
}

#[derive(Debug, Default)]
pub struct Bolus {
    pub status: u8,
    pub sequence: i32,
    pub delivered: i32,
    pub total: i32,
    pub extended_sequence: i32,
    pub extended_total: i32,
    pub extended_minutes: i32,
}

pub struct Pump {
    pub sim: YpsoSim,
    pub bolus: Bolus,
    pub step: i32,
    pub history: Vec<(u32, u8, [u16; 3])>,
    pub next_sequence: u32,
    pub deaf: bool,
    pub quiet: bool,
    pub grow: Vec<u8>,
    pub answer: Option<(pump_shared::Uuid, Vec<u8>)>,
    pub raw: Option<(pump_shared::Uuid, Vec<u8>)>,
    pub refuse_bolus: Option<u8>,
    pub corrupt: Option<u16>,
    pub bad_index: bool,
    selected: Option<u16>,
    index: u16,
    rx: Reassembler,
    tx: VecDeque<Vec<u8>>,
}

impl Device for Pump {
    fn sim(&mut self) -> &mut YpsoSim {
        &mut self.sim
    }
}

fn safevar(value: i32) -> Vec<u8> {
    [value.to_le_bytes(), (!value).to_le_bytes()].concat()
}

fn value_of(data: &[u8]) -> Option<i32> {
    let value = i32::from_le_bytes(data.get(..4)?.try_into().ok()?);
    (data.get(..8)? == safevar(value).as_slice()).then_some(value)
}

impl Pump {
    pub fn new(sim: YpsoSim) -> Self {
        Self {
            sim,
            bolus: Bolus::default(),
            step: 50,
            history: Vec::new(),
            next_sequence: 100,
            deaf: false,
            quiet: false,
            grow: Vec::new(),
            answer: None,
            raw: None,
            refuse_bolus: None,
            corrupt: None,
            bad_index: false,
            selected: None,
            index: 0,
            rx: Reassembler::default(),
            tx: VecDeque::new(),
        }
    }

    pub fn log(&mut self, kind: u8, values: [u16; 3]) -> u32 {
        self.next_sequence += 1;
        self.history.insert(0, (self.next_sequence, kind, values));
        self.next_sequence
    }

    fn seconds(&self, now: Timestamp) -> u32 {
        (self.sim.clock + now.as_millis() / 1000) as u32
    }

    fn reply(&mut self, plaintext: Vec<u8>) -> BleResult {
        if !self.sim.has_crypto_service() {
            return BleResult::Data(plaintext);
        }
        self.sim.pump_counter += 1;
        let sealed = Crypto::new(self.sim.key)
            .seal(
                &plaintext,
                self.sim.reboot,
                self.sim.pump_counter,
                &[3; NONCE_LEN],
            )
            .unwrap();
        self.tx = Frames::split(&sealed).unwrap().into();
        BleResult::Data(self.tx.pop_front().unwrap())
    }

    fn open(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, u8> {
        if !self.sim.has_crypto_service() {
            return Ok(Some(frame.to_vec()));
        }
        let message = match self.rx.push(frame) {
            Ok(Some(message)) => message,
            Ok(None) => return Ok(None),
            Err(_) => {
                self.rx.reset();
                return Err(0x8D);
            }
        };
        let opened = Crypto::new(self.sim.key).open(&message).map_err(|_| 0x8A)?;
        if opened.reboot != self.sim.reboot || opened.counter <= self.sim.app_counter {
            return Err(0x8B);
        }
        self.sim.app_counter = opened.counter;
        Ok(Some(opened.payload))
    }

    fn bolus_status(&mut self) -> Vec<u8> {
        let b = &mut self.bolus;
        if b.status == 1 {
            b.delivered = (b.delivered + self.step).min(b.total);
            if b.delivered == b.total {
                b.status = 4;
                let total = b.total as u16;
                if self.quiet {
                    self.log(16, [0, 0, 0]);
                } else {
                    self.log(2, [total, 0, 0]);
                }
            }
        }
        let b = &self.bolus;
        if self.sim.versions.control == "1.0" {
            let mut out = vec![b.status];
            for v in [b.delivered, b.total, b.sequence] {
                out.extend(v.to_le_bytes());
            }
            return out;
        }
        let mut out = vec![b.status];
        for v in [b.sequence, b.delivered, b.total] {
            out.extend(v.to_le_bytes());
        }
        out.push(u8::from(b.extended_sequence > 0));
        for v in [
            b.extended_sequence,
            0,
            b.extended_total,
            0,
            0,
            0,
            b.extended_minutes,
        ] {
            out.extend(v.to_le_bytes());
        }
        let crc = crc16(&out);
        out.extend(crc);
        out
    }

    fn apply(&mut self, characteristic: pump_shared::Uuid, data: &[u8], now: Timestamp) -> u8 {
        let le = |at: usize| i32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        match characteristic {
            uuids::SETTING_ID => match value_of(data).and_then(|v| u16::try_from(v).ok()) {
                Some(id) if self.sim.settings.contains_key(&id) => {
                    self.selected = Some(id);
                    0
                }
                _ => 0x83,
            },
            uuids::SETTING_VALUE => {
                let (Some(id), Some(value)) = (self.selected, value_of(data)) else {
                    return 0x81;
                };
                if !self.deaf {
                    self.sim.settings.insert(id, value);
                }
                0
            }
            uuids::START_STOP_TBR => {
                if !self.deaf {
                    self.sim.tbr_percent = le(0);
                }
                0
            }
            uuids::START_STOP_BOLUS => {
                if let Some(code) = self.refuse_bolus {
                    return code;
                }
                let (total, minutes, kind) = if data.len() == 8 {
                    (le(0), 0, 1)
                } else {
                    (le(0), le(4), data[12])
                };
                match (kind, total) {
                    (1, 0) => {
                        if self.bolus.status == 1 {
                            self.bolus.status = 4;
                            let delivered = self.bolus.delivered as u16;
                            self.log(29, [delivered, 0, 0]);
                        }
                    }
                    (1, _) => {
                        self.bolus = Bolus {
                            status: 1,
                            sequence: self.next_sequence as i32 + 1,
                            total,
                            ..std::mem::take(&mut self.bolus)
                        };
                        self.bolus.delivered = 0;
                    }
                    (_, 0) if !self.deaf => {
                        let total = self.bolus.extended_total as u16;
                        self.bolus.extended_sequence = 0;
                        self.log(30, [total, 0, 0]);
                    }
                    (_, 0) => {}
                    _ if self.deaf => {}
                    _ => {
                        self.bolus.extended_sequence = self.next_sequence as i32 + 1;
                        self.bolus.extended_total = total;
                        self.bolus.extended_minutes = minutes;
                        self.log(1, [total as u16, minutes as u16, 0]);
                    }
                }
                0
            }
            uuids::EVENT_ENTRY_INDEX => {
                let index = if data.len() == 2 {
                    i32::from(u16::from_le_bytes([data[0], data[1]]))
                } else {
                    value_of(data).unwrap_or(-1)
                };
                match u16::try_from(index) {
                    Ok(index) if usize::from(index) < self.history.len() => {
                        self.index = index;
                        0
                    }
                    _ => 0x84,
                }
            }
            uuids::SYSTEM_TIME | uuids::SYSTEM_DATE => {
                if characteristic == uuids::SYSTEM_TIME {
                    let of_day =
                        u64::from(data[0]) * 3600 + u64::from(data[1]) * 60 + u64::from(data[2]);
                    let now_secs = self.sim.clock + now.as_millis() / 1000;
                    self.sim.clock = now_secs - now_secs % 86_400 + of_day - now.as_millis() / 1000;
                }
                0
            }
            _ => 0x81,
        }
    }

    fn entry_value(&mut self, now: Timestamp) -> Vec<u8> {
        let index = self.index;
        self.index = self.index.wrapping_add(1);
        if index == 1 && !self.grow.is_empty() {
            for _ in 0..self.grow.remove(0) {
                self.log(16, [0, 0, 0]);
            }
        }
        let Some(&(sequence, kind, values)) = self.history.get(usize::from(index)) else {
            return vec![0; 3];
        };
        let reported = if self.bad_index { index + 7 } else { index };
        let mut bytes = entry(self.seconds(now), kind, values, sequence, reported);
        if self.corrupt == Some(index) {
            bytes[18] ^= 0xFF;
        }
        bytes
    }
}

const INTERCEPTED_WRITES: [pump_shared::Uuid; 7] = [
    uuids::SETTING_ID,
    uuids::SETTING_VALUE,
    uuids::START_STOP_TBR,
    uuids::START_STOP_BOLUS,
    uuids::EVENT_ENTRY_INDEX,
    uuids::SYSTEM_TIME,
    uuids::SYSTEM_DATE,
];

impl SimDevice for Pump {
    fn handle(&mut self, op: &BleOp, now: Timestamp) -> BleResult {
        if matches!(op, BleOp::Connect { .. } | BleOp::Disconnect) {
            self.rx.reset();
            self.tx.clear();
        }
        if !self.sim.is_connected() || matches!(op, BleOp::Connect { .. }) {
            return self.sim.handle(op, now);
        }
        match op {
            BleOp::Read { characteristic, .. }
                if self.raw.as_ref().is_some_and(|(c, _)| c == characteristic) =>
            {
                BleResult::Data(self.raw.as_ref().unwrap().1.clone())
            }
            BleOp::Read { characteristic, .. }
                if self
                    .answer
                    .as_ref()
                    .is_some_and(|(c, _)| c == characteristic) =>
            {
                let answer = self.answer.as_ref().unwrap().1.clone();
                self.reply(answer)
            }
            BleOp::Read { characteristic, .. } => match *characteristic {
                uuids::EXTENDED_READ if !self.tx.is_empty() => {
                    BleResult::Data(self.tx.pop_front().unwrap())
                }
                uuids::GET_BOLUS_STATUS => {
                    let status = self.bolus_status();
                    self.reply(status)
                }
                uuids::SETTING_VALUE => {
                    match self.selected.and_then(|id| self.sim.settings.get(&id)) {
                        Some(value) => {
                            let value = safevar(*value);
                            self.reply(value)
                        }
                        None => BleResult::Failed,
                    }
                }
                uuids::EVENT_ENTRY_COUNT => {
                    let count = safevar(self.history.len() as i32 + 2);
                    self.reply(count)
                }
                uuids::EVENT_ENTRY_VALUE => {
                    let value = self.entry_value(now);
                    self.reply(value)
                }
                _ => {
                    self.tx.clear();
                    self.sim.handle(op, now)
                }
            },
            BleOp::Write {
                characteristic,
                data,
                ..
            } if INTERCEPTED_WRITES.contains(characteristic) => match self.open(data) {
                Ok(None) => BleResult::Done,
                Ok(Some(payload)) => match self.apply(*characteristic, &payload, now) {
                    0 => BleResult::Done,
                    code => BleResult::Status(code),
                },
                Err(code) => BleResult::Status(code),
            },
            _ => self.sim.handle(op, now),
        }
    }
}

pub fn finished(events: &[Event]) -> Vec<(CommandId, Outcome)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Finished { id, outcome } => Some((*id, outcome.clone())),
            _ => None,
        })
        .collect()
}
