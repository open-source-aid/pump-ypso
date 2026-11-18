use std::time::Duration;

use pump_shared::{
    Address, Advertisement, Command, CommandId, Config, Driver, Engine, Event, HistoryEntry,
    InsulinAmount, Outcome, PumpStatus, Report, Sequence, Slot,
};
use pump_ypso::protocol::{setting, uuids, KEY_LEN};
use pump_ypso::sim::{YpsoSim, PASSWORD_LEN};
use pump_ypso::DRIVER;
use tester_harness::{Harness, SimDevice};

pub const KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
pub const PASSWORD: [u8; PASSWORD_LEN] = [0x5A; PASSWORD_LEN];
pub const SERIAL: u32 = 10_155_555;
pub const MAC: [u8; 6] = [0xEC, 0x2A, 0xF0, 0x01, 0x02, 0x03];
pub const NOON: i64 = 9497 * 86_400 + 12 * 3600 + 60;
pub const Y2K: u64 = 946_684_800;

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

pub struct Blob {
    pub version: u8,
    pub flags: u8,
    pub credentials: Vec<(Vec<u8>, [u8; PASSWORD_LEN])>,
}

impl Default for Blob {
    fn default() -> Self {
        Self {
            version: 1,
            flags: 0,
            credentials: vec![(b"O2".to_vec(), PASSWORD)],
        }
    }
}

impl Blob {
    pub fn opn() -> Self {
        Self {
            version: 4,
            flags: 1,
            ..Self::default()
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = b"YB".to_vec();
        out.extend([self.version, self.flags]);
        out.extend(MAC);
        out.extend(SERIAL.to_le_bytes());
        out.extend(KEY);
        out.extend((-1i64).to_le_bytes());
        out.extend((-1i64).to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(0i64.to_le_bytes());
        out.push(0);
        out.extend([7; 16]);
        out.push(self.credentials.len() as u8);
        for (code, password) in &self.credentials {
            out.push(code.len() as u8);
            out.extend(code);
            out.extend(password);
        }
        let crc = crc32(&out);
        out.extend(crc.to_le_bytes());
        out
    }
}

pub fn harness<D: SimDevice>(device: D, blob: &Blob) -> Harness<D> {
    let mut harness = Harness::new(device);
    harness.announced = Some(DRIVER.info());
    harness.resendable.insert(uuids::AUTHORIZATION_PASSWORD);
    harness.resendable.insert(uuids::SETTING_ID);
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
        (setting::BOLUS_STEP_SELECTION, 5),
        (setting::BASAL_RATE_MAX, 2_500),
        (setting::TBR_SHORT_SCALE_MAX, 250),
        (setting::TBR_SHORT_DURATION_MAX, 120),
    ] {
        sim.settings.insert(id, value);
    }
    for hour in 0..setting::BASAL_SLOTS {
        sim.settings.insert(setting::basal_a(hour), 80);
    }
    sim
}

pub fn units(milli: u32) -> InsulinAmount {
    InsulinAmount::from_milliunits(milli)
}

pub struct Run<D> {
    pub harness: Harness<D>,
    pub engine: Box<dyn Engine>,
}

impl<D: SimDevice> Run<D> {
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
        loop {
            if let Some(outcome) = self.outcome(id) {
                return outcome;
            }
            self.settle();
        }
    }

    pub fn status(&mut self) -> PumpStatus {
        match self.run(Command::Status) {
            Outcome::Completed(Report::Status(status)) => status,
            other => panic!("not a status: {other:?}"),
        }
    }

    pub fn history(&mut self, after: u32) -> Vec<HistoryEntry> {
        match self.run(Command::ReadHistory {
            after: Sequence::new(after),
        }) {
            Outcome::Completed(Report::History(entries)) => entries,
            other => panic!("{other:?}"),
        }
    }

    pub fn restart(&mut self) {
        self.engine = engine();
        self.harness.events.clear();
        self.harness
            .run(self.engine.as_mut())
            .expect("no violation");
    }
}
