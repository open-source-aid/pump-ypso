#![cfg(feature = "sim")]

mod auth;
mod history;
mod link;
#[cfg(feature = "opn")]
mod opn;
mod pairing;
mod settings;
mod therapy;
mod transport;

use pump_shared::{Address, BleOp, BleResult, Timestamp, Uuid};
use pump_testkit::SimDevice;
use pump_ypso::protocol::{uuids, Characteristic, KEY_LEN, NONCE_LEN};
use pump_ypso::sim::{crc16, YpsoSim, PASSWORD_LEN};
use pump_ypso::{Crypto, Frames, Opened, ProtoError, Reassembler};

pub const KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
pub const PASSWORD: [u8; PASSWORD_LEN] = [0x5A; PASSWORD_LEN];

pub fn connect() -> BleOp {
    BleOp::Connect {
        address: Address::new([0xEC, 0x2A, 0xF0, 0x01, 0x02, 0x03]),
    }
}

pub fn read(service: Uuid, characteristic: Uuid) -> BleOp {
    BleOp::Read {
        service,
        characteristic,
    }
}

pub fn write(service: Uuid, characteristic: Uuid, data: &[u8]) -> BleOp {
    BleOp::Write {
        service,
        characteristic,
        data: data.to_vec(),
    }
}

pub fn ids(characteristic: Characteristic) -> (Uuid, Uuid) {
    (
        characteristic.service().unwrap().uuid(),
        characteristic.uuid().unwrap(),
    )
}

pub fn safevar(value: i32) -> Vec<u8> {
    [value.to_le_bytes(), (!value).to_le_bytes()].concat()
}

pub fn le_i32(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub fn bolus(total: i32, minutes: i32, immediate: i32, kind: u8) -> Vec<u8> {
    let mut bytes = [
        total.to_le_bytes(),
        minutes.to_le_bytes(),
        immediate.to_le_bytes(),
    ]
    .concat();
    bytes.push(kind);
    let crc = crc16(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes
}

pub fn tbr(percent: i32, minutes: i32) -> Vec<u8> {
    [safevar(percent), safevar(minutes)].concat()
}

pub struct App {
    pub sim: YpsoSim,
    pub crypto: Option<Crypto>,
    pub reboot: u32,
    pub counter: u64,
    pub now: u64,
}

impl App {
    pub fn new(sim: YpsoSim) -> Self {
        let crypto = sim
            .key
            .filter(|_| sim.has_crypto_service())
            .map(Crypto::new);
        let mut app = Self {
            sim,
            crypto,
            reboot: 0,
            counter: 0,
            now: 0,
        };
        assert_eq!(app.op(connect()), BleResult::Done);
        app
    }

    pub fn authorised(sim: YpsoSim) -> Self {
        let mut app = Self::new(sim);
        assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
        app
    }

    pub fn connected() -> Self {
        Self::authorised(YpsoSim::new(KEY, PASSWORD))
    }

    pub fn op(&mut self, op: BleOp) -> BleResult {
        self.sim.handle(&op, Timestamp::from_millis(self.now))
    }

    pub fn at(&self) -> Timestamp {
        Timestamp::from_millis(self.now)
    }

    pub fn wait(&mut self, millis: u64) {
        self.now += millis;
        self.op(BleOp::RequestFastInterval);
    }

    pub fn authorise(&mut self, password: &[u8]) -> BleResult {
        let (service, characteristic) = ids(Characteristic::AuthorizationPassword);
        self.op(write(service, characteristic, password))
    }

    pub fn frames(&mut self, payload: &[u8]) -> Vec<Vec<u8>> {
        let Some(crypto) = &self.crypto else {
            return vec![payload.to_vec()];
        };
        self.counter += 1;
        let sealed = crypto
            .seal(payload, self.reboot, self.counter, &[7; NONCE_LEN])
            .unwrap();
        Frames::split(&sealed).unwrap()
    }

    pub fn write_all(&mut self, characteristic: Characteristic, payload: &[u8]) -> Vec<BleResult> {
        let (service, uuid) = ids(characteristic);
        self.frames(payload)
            .into_iter()
            .map(|frame| self.op(write(service, uuid, &frame)))
            .collect()
    }

    pub fn send(&mut self, characteristic: Characteristic, payload: &[u8]) -> BleResult {
        let results = self.write_all(characteristic, payload);
        let (last, rest) = results.split_last().unwrap();
        assert!(rest.iter().all(|r| *r == BleResult::Done), "{results:?}");
        last.clone()
    }

    pub fn read_frames(
        &mut self,
        service: Uuid,
        characteristic: Uuid,
    ) -> Result<Vec<Vec<u8>>, BleResult> {
        let first = match self.op(read(service, characteristic)) {
            BleResult::Data(first) => first,
            other => return Err(other),
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
        Ok(frames)
    }

    pub fn reassemble(
        &mut self,
        service: Uuid,
        characteristic: Uuid,
    ) -> Result<Vec<u8>, BleResult> {
        let mut reassembler = Reassembler::default();
        let mut message = None;
        for frame in self.read_frames(service, characteristic)? {
            message = reassembler.push(&frame).unwrap();
        }
        Ok(message.unwrap())
    }

    pub fn opened(&mut self, characteristic: Characteristic) -> Result<Opened, ProtoError> {
        let (service, uuid) = ids(characteristic);
        let message = self.reassemble(service, uuid).unwrap();
        self.crypto.as_ref().unwrap().open(&message)
    }

    pub fn get(&mut self, characteristic: Characteristic) -> Result<Vec<u8>, BleResult> {
        let (service, uuid) = ids(characteristic);
        if self.crypto.is_none() {
            return match self.op(read(service, uuid)) {
                BleResult::Data(data) => Ok(data),
                other => Err(other),
            };
        }
        let message = self.reassemble(service, uuid)?;
        Ok(self
            .crypto
            .as_ref()
            .unwrap()
            .open(&message)
            .unwrap()
            .payload)
    }

    pub fn setting(&mut self, id: u16) -> i32 {
        assert_eq!(
            self.send(Characteristic::SettingId, &safevar(i32::from(id))),
            BleResult::Done
        );
        let value = self.get(Characteristic::SettingValue).unwrap();
        assert_eq!(value[4..], (!le_i32(&value, 0)).to_le_bytes());
        le_i32(&value, 0)
    }

    pub fn system_status(&mut self) -> Vec<u8> {
        self.get(Characteristic::GetSystemStatus).unwrap()
    }

    pub fn bolus_status(&mut self) -> Vec<u8> {
        self.get(Characteristic::GetBolusStatus).unwrap()
    }

    pub fn events(&self) -> Vec<(u8, [u16; 3])> {
        self.sim
            .events
            .entries
            .iter()
            .map(|entry| (entry.kind, entry.values))
            .collect()
    }
}
