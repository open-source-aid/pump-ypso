use std::collections::{BTreeMap, VecDeque};

use pump_shared::{BleOp, BleResult, Timestamp, Uuid};
use pump_testkit::SimDevice;

use crate::protocol::{
    setting, uuids, Crypto, Frames, Reassembler, Service, ATT_INSUFFICIENT_AUTHORIZATION,
    ERR_COUNTER_ERROR, ERR_ENCRYPTION_FAILED, ERR_INVALID_ID, ERR_VALUE_CORRUPT, KEY_LEN,
    NONCE_LEN,
};

pub const PASSWORD_LEN: usize = 16;

const SECONDS_PER_DAY: u64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versions {
    pub pump_base: String,
    pub master_software: String,
    pub settings: String,
    pub history: String,
    pub control: String,
}

#[derive(Debug)]
pub struct YpsoSim {
    pub versions: Versions,
    pub password: [u8; PASSWORD_LEN],
    pub key: [u8; KEY_LEN],
    pub reboot: u32,
    pub app_counter: u64,
    pub pump_counter: u64,
    pub delivery_state: u8,
    pub reservoir: i32,
    pub battery: u8,
    pub basal_rate: i32,
    pub tbr_percent: i32,
    pub clock: u64,
    pub settings: BTreeMap<u16, i32>,
    pub alarm_count: i32,
    crypto_service: bool,
    connected: bool,
    selected: Option<u16>,
    rx: Reassembler,
    tx: VecDeque<Vec<u8>>,
}

enum Reader {
    Clear(fn(&YpsoSim) -> &str),
    Sealed(fn(&YpsoSim, u64) -> Option<Vec<u8>>),
}

const READS: [(Service, Uuid, Reader); 10] = [
    (
        Service::PumpBase,
        uuids::PUMP_BASE_SERVICE_VERSION,
        Reader::Clear(|sim| &sim.versions.pump_base),
    ),
    (
        Service::PumpBase,
        uuids::MASTER_SOFTWARE_VERSION,
        Reader::Clear(|sim| &sim.versions.master_software),
    ),
    (
        Service::Settings,
        uuids::SETTINGS_SERVICE_VERSION,
        Reader::Clear(|sim| &sim.versions.settings),
    ),
    (
        Service::History,
        uuids::HISTORY_SERVICE_VERSION,
        Reader::Clear(|sim| &sim.versions.history),
    ),
    (
        Service::Control,
        uuids::CONTROL_SERVICE_VERSION,
        Reader::Clear(|sim| &sim.versions.control),
    ),
    (
        Service::Control,
        uuids::GET_SYSTEM_STATUS,
        Reader::Sealed(|sim, _| Some(sim.system_status())),
    ),
    (
        Service::Settings,
        uuids::SETTING_VALUE,
        Reader::Sealed(|sim, _| {
            sim.selected
                .and_then(|id| sim.settings.get(&id))
                .map(|value| safevar(*value).to_vec())
        }),
    ),
    (
        Service::Settings,
        uuids::SYSTEM_DATE,
        Reader::Sealed(|_, seconds| {
            let (year, month, day) = civil_date(seconds / SECONDS_PER_DAY);
            let [lo, hi] = year.to_le_bytes();
            Some(vec![lo, hi, month, day])
        }),
    ),
    (
        Service::Settings,
        uuids::SYSTEM_TIME,
        Reader::Sealed(|_, seconds| {
            let of_day = seconds % SECONDS_PER_DAY;
            Some(
                [of_day / 3600, of_day / 60 % 60, of_day % 60]
                    .map(|part| part as u8)
                    .to_vec(),
            )
        }),
    ),
    (
        Service::History,
        uuids::ALARM_ENTRY_COUNT,
        Reader::Sealed(|sim, _| Some(safevar(sim.alarm_count).to_vec())),
    ),
];

enum Writer {
    Clear(fn(&mut YpsoSim, &[u8]) -> u8),
    Sealed(fn(&mut YpsoSim, &[u8]) -> u8),
}

const WRITES: [(Service, Uuid, Writer); 2] = [
    (
        Service::PumpBase,
        uuids::AUTHORIZATION_PASSWORD,
        Writer::Clear(|sim, data| {
            if data == sim.password {
                0
            } else {
                ATT_INSUFFICIENT_AUTHORIZATION
            }
        }),
    ),
    (
        Service::Settings,
        uuids::SETTING_ID,
        Writer::Sealed(|sim, data| {
            let Some(id) = parse_safevar(data) else {
                return ERR_VALUE_CORRUPT;
            };
            match u16::try_from(id) {
                Ok(id) if sim.settings.contains_key(&id) => {
                    sim.selected = Some(id);
                    0
                }
                _ => ERR_INVALID_ID,
            }
        }),
    ),
];

impl YpsoSim {
    pub fn new(key: [u8; KEY_LEN], password: [u8; PASSWORD_LEN]) -> Self {
        Self {
            versions: Versions {
                pump_base: "1.2".into(),
                master_software: "V05.02.03".into(),
                settings: "1.7".into(),
                history: "1.4".into(),
                control: "1.4".into(),
            },
            password,
            key,
            reboot: 0,
            app_counter: 0,
            pump_counter: 0,
            delivery_state: 10,
            reservoir: 15_000,
            battery: 4,
            basal_rate: 52,
            tbr_percent: 100,
            clock: 0,
            settings: BTreeMap::from([
                (setting::SERIAL_NUMBER_LOWER, 63_011),
                (setting::SERIAL_NUMBER_UPPER, 154),
            ]),
            alarm_count: 0,
            crypto_service: true,
            connected: false,
            selected: None,
            rx: Reassembler::default(),
            tx: VecDeque::new(),
        }
    }

    #[cfg(feature = "opn")]
    pub fn opn(password: [u8; PASSWORD_LEN]) -> Self {
        Self {
            versions: Versions {
                pump_base: "1.1".into(),
                master_software: "V65.00.06".into(),
                settings: "1.3".into(),
                history: "1.1".into(),
                control: "1.0".into(),
            },
            crypto_service: false,
            ..Self::new([0; KEY_LEN], password)
        }
    }

    pub fn has_crypto_service(&self) -> bool {
        self.crypto_service
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn reboot(&mut self) {
        self.reboot += 1;
        self.drop_link();
    }

    fn drop_link(&mut self) {
        self.connected = false;
        self.rx.reset();
        self.tx.clear();
    }

    fn system_status(&self) -> Vec<u8> {
        let mut status = vec![self.delivery_state];
        status.extend_from_slice(&self.reservoir.to_le_bytes());
        status.push(self.battery);
        status.extend_from_slice(&self.basal_rate.to_le_bytes());
        status.extend_from_slice(&self.tbr_percent.to_le_bytes());
        status
    }

    fn read(&mut self, service: Uuid, characteristic: Uuid, now: Timestamp) -> BleResult {
        if self.crypto_service
            && service == Service::Crypto.uuid()
            && characteristic == uuids::EXTENDED_READ
        {
            return self
                .tx
                .pop_front()
                .map_or(BleResult::Failed, BleResult::Data);
        }
        let Some((_, _, reader)) = READS
            .iter()
            .find(|(s, c, _)| s.uuid() == service && *c == characteristic)
        else {
            return BleResult::Failed;
        };
        let seconds = self.clock + now.as_millis() / 1000;
        match reader {
            Reader::Clear(version) => BleResult::Data(version(self).as_bytes().to_vec()),
            Reader::Sealed(value) => {
                value(self, seconds).map_or(BleResult::Failed, |plaintext| self.reply(plaintext))
            }
        }
    }

    fn reply(&mut self, plaintext: Vec<u8>) -> BleResult {
        if !self.crypto_service {
            return BleResult::Data(plaintext);
        }
        self.pump_counter += 1;
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..8].copy_from_slice(&self.pump_counter.to_le_bytes());
        nonce[8..12].copy_from_slice(&self.reboot.to_le_bytes());
        let sealed = Crypto::new(self.key)
            .seal(&plaintext, self.reboot, self.pump_counter, &nonce)
            .expect("every simulated reply fits one message");
        self.tx = Frames::split(&sealed)
            .expect("every simulated reply fits the frames")
            .into();
        BleResult::Data(self.tx.pop_front().expect("a split yields a frame"))
    }

    fn write(&mut self, service: Uuid, characteristic: Uuid, data: &[u8]) -> BleResult {
        let Some((_, _, writer)) = WRITES
            .iter()
            .find(|(s, c, _)| s.uuid() == service && *c == characteristic)
        else {
            return BleResult::Failed;
        };
        match writer {
            Writer::Sealed(apply) if self.crypto_service => self.sealed_write(*apply, data),
            Writer::Clear(apply) | Writer::Sealed(apply) => status(apply(self, data)),
        }
    }

    fn sealed_write(&mut self, apply: fn(&mut Self, &[u8]) -> u8, frame: &[u8]) -> BleResult {
        match self.rx.push(frame) {
            Ok(None) => BleResult::Done,
            Ok(Some(message)) => match self.open(&message) {
                Ok(plaintext) => status(apply(self, &plaintext)),
                Err(code) => BleResult::Status(code),
            },
            Err(_) => {
                self.rx.reset();
                BleResult::Failed
            }
        }
    }

    fn open(&mut self, message: &[u8]) -> Result<Vec<u8>, u8> {
        let opened = Crypto::new(self.key)
            .open(message)
            .map_err(|_| ERR_ENCRYPTION_FAILED)?;
        if opened.reboot != self.reboot || opened.counter as i64 <= self.app_counter as i64 {
            return Err(ERR_COUNTER_ERROR);
        }
        self.app_counter = opened.counter;
        Ok(opened.payload)
    }
}

impl SimDevice for YpsoSim {
    fn handle(&mut self, op: &BleOp, now: Timestamp) -> BleResult {
        match op {
            BleOp::Connect { .. } => {
                self.drop_link();
                self.connected = true;
                BleResult::Done
            }
            _ if !self.connected => BleResult::Disconnected,
            BleOp::Disconnect => {
                self.drop_link();
                BleResult::Done
            }
            BleOp::RequestFastInterval | BleOp::Subscribe { .. } => BleResult::Done,
            BleOp::Read {
                service,
                characteristic,
            } => self.read(*service, *characteristic, now),
            BleOp::Write {
                service,
                characteristic,
                data,
            } => self.write(*service, *characteristic, data),
        }
    }
}

fn status(code: u8) -> BleResult {
    match code {
        0 => BleResult::Done,
        code => BleResult::Status(code),
    }
}

fn safevar(value: i32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&value.to_le_bytes());
    bytes[4..].copy_from_slice(&(!value).to_le_bytes());
    bytes
}

fn parse_safevar(data: &[u8]) -> Option<i32> {
    let bytes: [u8; 8] = data.try_into().ok()?;
    let value = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (bytes == safevar(value)).then_some(value)
}

fn civil_date(days_since_2000: u64) -> (u16, u8, u8) {
    let shifted = days_since_2000 + 730_425;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = (shifted_month + 2) % 12 + 1;
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year as u16, month as u8, day as u8)
}
