use alloc::vec::Vec;

use pump_shared::{BleOp, BleResult, KeyRef, SimDevice, Timestamp, Uuid};

use super::codec::status;
use super::YpsoSim;
use crate::protocol::{uuids, Characteristic, Service};

const ATT_WRITE_NOT_PERMITTED: u8 = 0x02;
const ATT_INVALID_ATTRIBUTE_VALUE_LENGTH: u8 = 0x0D;

type Read = fn(&mut YpsoSim, u64) -> Result<Vec<u8>, BleResult>;
type Write = fn(&mut YpsoSim, &[u8], u64) -> BleResult;

const SERVED: [Characteristic; 29] = [
    Characteristic::PumpBaseServiceVersion,
    Characteristic::MasterSoftwareVersion,
    Characteristic::SupervisorSoftwareVersion,
    Characteristic::AuthorizationPassword,
    Characteristic::SettingsServiceVersion,
    Characteristic::SettingId,
    Characteristic::SettingValue,
    Characteristic::SystemDate,
    Characteristic::SystemTime,
    Characteristic::HistoryServiceVersion,
    Characteristic::AlarmEntryCount,
    Characteristic::AlarmEntryIndex,
    Characteristic::AlarmEntryValue,
    Characteristic::EventEntryCount,
    Characteristic::EventEntryIndex,
    Characteristic::EventEntryValue,
    Characteristic::SystemEntryCount,
    Characteristic::ComplaintEntryCount,
    Characteristic::SystemEntryIndex,
    Characteristic::ComplaintEntryIndex,
    Characteristic::SystemEntryValue,
    Characteristic::ComplaintEntryValue,
    Characteristic::CounterId,
    Characteristic::CounterValue,
    Characteristic::ControlServiceVersion,
    Characteristic::StartStopBolus,
    Characteristic::GetBolusStatus,
    Characteristic::StartStopTbr,
    Characteristic::GetSystemStatus,
];

fn served(service: Uuid, characteristic: Uuid) -> Option<Characteristic> {
    SERVED.into_iter().find(|served| {
        served.uuid() == Some(characteristic) && served.service().map(|s| s.uuid()) == Some(service)
    })
}

fn reader(characteristic: Characteristic) -> Option<Read> {
    Some(match characteristic {
        Characteristic::SettingValue => YpsoSim::setting_value,
        Characteristic::SystemDate => YpsoSim::date,
        Characteristic::SystemTime => YpsoSim::time,
        Characteristic::CounterValue => YpsoSim::counter_value,
        Characteristic::AlarmEntryCount => |sim, _| Ok(sim.alarms.count()),
        Characteristic::EventEntryCount => |sim, _| Ok(sim.events.count()),
        Characteristic::SystemEntryCount => |sim, _| Ok(sim.system.count()),
        Characteristic::ComplaintEntryCount => |sim, _| Ok(sim.complaints.count()),
        Characteristic::AlarmEntryValue => |sim, _| sim.alarms.value(),
        Characteristic::EventEntryValue => |sim, _| sim.events.value(),
        Characteristic::SystemEntryValue => |sim, _| sim.system.value(),
        Characteristic::ComplaintEntryValue => |sim, _| sim.complaints.value(),
        Characteristic::GetBolusStatus => |sim, now| Ok(sim.bolus_status(now)),
        Characteristic::GetSystemStatus => |sim, now| Ok(sim.system_status(now)),
        _ => return None,
    })
}

fn writer(characteristic: Characteristic) -> Option<Write> {
    Some(match characteristic {
        Characteristic::SettingId => |sim, data, _| sim.select_setting(data),
        Characteristic::SettingValue => YpsoSim::write_setting,
        Characteristic::SystemDate => YpsoSim::write_date,
        Characteristic::SystemTime => YpsoSim::write_time,
        Characteristic::CounterId => |sim, data, _| sim.select_counter(data),
        Characteristic::AlarmEntryIndex => |sim, data, _| {
            let wide = sim.safevar_index();
            sim.alarms.select(data, wide)
        },
        Characteristic::EventEntryIndex => |sim, data, _| {
            let wide = sim.safevar_index();
            sim.events.select(data, wide)
        },
        Characteristic::SystemEntryIndex => |sim, data, _| {
            let wide = sim.safevar_index();
            sim.system.select(data, wide)
        },
        Characteristic::ComplaintEntryIndex => |sim, data, _| {
            let wide = sim.safevar_index();
            sim.complaints.select(data, wide)
        },
        Characteristic::StartStopBolus => YpsoSim::bolus_command,
        Characteristic::StartStopTbr => YpsoSim::tbr_command,
        _ => return None,
    })
}

impl YpsoSim {
    pub(super) fn drop_link(&mut self) {
        self.connected = false;
        self.authorised = false;
        self.selected = 0;
        self.rx = None;
        self.tx.clear();
    }

    fn version(&self, characteristic: Characteristic) -> Option<&[u8]> {
        Some(match characteristic {
            Characteristic::PumpBaseServiceVersion => self.versions.pump_base.as_bytes(),
            Characteristic::MasterSoftwareVersion => self.versions.master_software.as_bytes(),
            Characteristic::SupervisorSoftwareVersion => &self.supervisor,
            Characteristic::SettingsServiceVersion => self.versions.settings.as_bytes(),
            Characteristic::HistoryServiceVersion => self.versions.history.as_bytes(),
            Characteristic::ControlServiceVersion => self.versions.control.as_bytes(),
            _ => return None,
        })
    }

    fn read(&mut self, service: Uuid, characteristic: Uuid, now: u64) -> BleResult {
        if self.crypto_service && service == uuids::CRYPTO_SERVICE {
            return match characteristic {
                uuids::EXTENDED_READ => self.continuation(),
                uuids::NEW_PUBLIC_KEY => self.offer(now),
                _ => BleResult::Failed,
            };
        }
        let Some(characteristic) = served(service, characteristic) else {
            return BleResult::Failed;
        };
        if let Some(text) = self.version(characteristic) {
            return BleResult::Data(text.to_vec());
        }
        let Some(read) = reader(characteristic) else {
            return BleResult::Failed;
        };
        if let Some(code) = self.gate(characteristic, now) {
            return BleResult::Status(code);
        }
        match read(self, now) {
            Ok(plaintext) => self.reply(plaintext),
            Err(result) => result,
        }
    }

    fn write(&mut self, service: Uuid, characteristic: Uuid, data: &[u8], now: u64) -> BleResult {
        if data.is_empty() {
            return BleResult::Status(ATT_INVALID_ATTRIBUTE_VALUE_LENGTH);
        }
        if self.crypto_service
            && service == uuids::CRYPTO_SERVICE
            && characteristic == uuids::WRITE_CHALLENGE_UTC_APP_PUB_KEY
        {
            return match self.receive(characteristic, data) {
                Ok(Some(blob)) => status(self.exchange(&blob, now)),
                Ok(None) => BleResult::Done,
                Err(result) => result,
            };
        }
        let Some(target) = served(service, characteristic) else {
            return BleResult::Failed;
        };
        if let Some(code) = self.security() {
            return BleResult::Status(code);
        }
        if target == Characteristic::AuthorizationPassword {
            return self.authorise(data);
        }
        let Some(write) = writer(target) else {
            return match (target.service(), self.access()) {
                (Some(Service::PumpBase), Some(_)) => BleResult::Status(ATT_WRITE_NOT_PERMITTED),
                _ => BleResult::Failed,
            };
        };
        let message = match self.message(characteristic, data) {
            Ok(Some(message)) => message,
            Ok(None) => return BleResult::Done,
            Err(result) => return result,
        };
        if let Some(code) = self.gate(target, now) {
            return BleResult::Status(code);
        }
        match self.open(message) {
            Ok(plaintext) => write(self, &plaintext, now),
            Err(code) => BleResult::Status(code),
        }
    }
}

impl SimDevice for YpsoSim {
    fn handle(&mut self, op: &BleOp, now: Timestamp) -> BleResult {
        let now = now.as_millis();
        self.advance(now);
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
            BleOp::RequestFastInterval | BleOp::Subscribe { .. } | BleOp::Unsubscribe { .. } => {
                BleResult::Done
            }
            BleOp::Read {
                service,
                characteristic,
            } => self.read(*service, *characteristic, now),
            BleOp::Write {
                service,
                characteristic,
                data,
            } => self.write(*service, *characteristic, data, now),
        }
    }

    fn key_agree(&mut self, _key: &KeyRef, peer_public: &[u8; 32]) -> Option<[u8; 32]> {
        self.crypto_service
            .then(|| super::pairing::shared(self.server_secret, *peer_public))
    }
}
