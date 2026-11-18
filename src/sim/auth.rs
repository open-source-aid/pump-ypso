use pump_shared::{BleResult, Timestamp};

use super::YpsoSim;
use crate::protocol::{
    Characteristic, ATT_INSUFFICIENT_AUTHENTICATION, ATT_INSUFFICIENT_AUTHORIZATION,
    ATT_INSUFFICIENT_ENCRYPTION,
};

pub(super) const ERR_BLOCKED: u8 = 0x88;
pub(super) const ERR_NOT_READY: u8 = 0x89;
const NOT_READY_MILLIS: u64 = 45_000;

impl YpsoSim {
    pub fn reboot(&mut self, now: Timestamp) {
        self.reboot += 1;
        self.ready_at = now.as_millis() + NOT_READY_MILLIS;
        self.drop_link();
    }

    pub(super) fn authorise(&mut self, data: &[u8]) -> BleResult {
        self.authorised |= data == self.password;
        BleResult::Done
    }

    pub(super) fn security(&self) -> Option<u8> {
        if !self.encrypted_link {
            Some(ATT_INSUFFICIENT_ENCRYPTION)
        } else if !self.bonded {
            Some(ATT_INSUFFICIENT_AUTHENTICATION)
        } else {
            None
        }
    }

    pub(super) fn access(&self) -> Option<u8> {
        (!self.authorised).then_some(ATT_INSUFFICIENT_AUTHORIZATION)
    }

    pub(super) fn pause(&self, now: u64) -> Option<u8> {
        if now < self.ready_at {
            Some(ERR_NOT_READY)
        } else if now < self.cooldown_until {
            Some(ERR_BLOCKED)
        } else {
            None
        }
    }

    pub(super) fn gate(&self, characteristic: Characteristic, now: u64) -> Option<u8> {
        let exempt = matches!(
            characteristic,
            Characteristic::SettingId
                | Characteristic::SettingValue
                | Characteristic::StartStopBolus
                | Characteristic::StartStopTbr
        );
        self.security()
            .or_else(|| self.access().filter(|_| !exempt))
            .or_else(|| self.pause(now).filter(|_| self.crypto_service))
    }
}
