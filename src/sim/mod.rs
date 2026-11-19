mod auth;
mod codec;
mod history;
mod link;
#[cfg(feature = "opn")]
mod opn;
mod pairing;
mod settings;
mod therapy;
mod transport;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use crate::protocol::{setting, KEY_LEN};

pub use crate::engine::crc::crc16;
pub use history::{Entry, Log};
pub use therapy::{Alarm, BOLUS_AMOUNT_CAP};

pub const PASSWORD_LEN: usize = 16;
pub const SUPERVISOR_LEN: usize = 10;

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
    pub key: Option<[u8; KEY_LEN]>,
    pub supervisor: [u8; SUPERVISOR_LEN],
    pub encrypted_link: bool,
    pub bonded: bool,
    pub reboot: u32,
    pub app_counter: u64,
    pub pump_counter: u64,
    pub reservoir: i32,
    pub delivered_today: i32,
    pub battery: u8,
    pub clock: i64,
    pub settings: BTreeMap<u16, i32>,
    pub counters: BTreeMap<u16, i32>,
    pub events: Log,
    pub alarms: Log,
    pub system: Log,
    pub complaints: Log,
    pub bolus_millis_per_hundredth: u64,
    pub server_secret: [u8; KEY_LEN],
    pub pump_secret: [u8; KEY_LEN],
    pub challenge: [u8; KEY_LEN],
    pub pairing_counter: u64,
    crypto_service: bool,
    connected: bool,
    authorised: bool,
    ready_at: u64,
    cooldown_until: u64,
    selected: i32,
    counter: Option<u16>,
    rx: Option<transport::Inbox>,
    tx: VecDeque<Vec<u8>>,
    last: u64,
    basal_carry: i32,
    alarm_active: bool,
    bolus: Option<therapy::Immediate>,
    slow: Option<therapy::Slow>,
    temp: Option<therapy::Temp>,
}

impl YpsoSim {
    pub fn new(key: [u8; KEY_LEN], password: [u8; PASSWORD_LEN]) -> Self {
        let mut settings = BTreeMap::from([
            (setting::BASAL_PROFILE_SELECTION, 3),
            (setting::PUMP_MODE_SELECTION, therapy::RUNNING),
            (setting::BOLUS_STEP_SELECTION, 10),
            (setting::SERIAL_NUMBER_LOWER, 63_011),
            (setting::SERIAL_NUMBER_UPPER, 154),
            (BOLUS_AMOUNT_CAP, 2_500),
            (setting::BASAL_RATE_CAP, 4_000),
            (setting::BASAL_RATE_MIN, 2),
            (setting::BASAL_RATE_MAX, 4_000),
            (setting::TBR_LONG_SCALE_MAX, 200),
            (setting::TBR_LONG_DURATION_MAX, 240),
            (setting::TBR_SHORT_SCALE_MAX, 200),
            (setting::TBR_SHORT_DURATION_MAX, 240),
            (setting::TBR_END_NOT_INDICATED, 1),
        ]);
        for hour in 0..setting::BASAL_SLOTS {
            settings.insert(setting::basal_a(hour), 52);
            settings.insert(setting::basal_b(hour), 40);
        }
        Self {
            versions: Versions {
                pump_base: "1.2".into(),
                master_software: "V05.02.03".into(),
                settings: "1.7".into(),
                history: "1.4".into(),
                control: "1.4".into(),
            },
            password,
            key: Some(key),
            supervisor: *b"V05.02.03\0",
            encrypted_link: true,
            bonded: true,
            reboot: 0,
            app_counter: 0,
            pump_counter: 0,
            reservoir: 15_000,
            delivered_today: 0,
            battery: 4,
            clock: 0,
            settings,
            counters: (1..=17).map(|id| (id, 0)).collect(),
            events: Log::default(),
            alarms: Log::default(),
            system: Log::default(),
            complaints: Log::default(),
            bolus_millis_per_hundredth: 87,
            server_secret: [0x11; KEY_LEN],
            pump_secret: [0x22; KEY_LEN],
            challenge: [0x5A; KEY_LEN],
            pairing_counter: 0,
            crypto_service: true,
            connected: false,
            authorised: false,
            ready_at: 0,
            cooldown_until: 0,
            selected: 0,
            counter: None,
            rx: None,
            tx: VecDeque::new(),
            last: 0,
            basal_carry: 0,
            alarm_active: false,
            bolus: None,
            slow: None,
            temp: None,
        }
    }

    pub fn has_crypto_service(&self) -> bool {
        self.crypto_service
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn is_authorised(&self) -> bool {
        self.authorised
    }

    fn setting(&self, id: u16) -> i32 {
        self.settings.get(&id).copied().unwrap_or_default()
    }

    fn isc(&self) -> bool {
        codec::version(&self.versions.control) > 1.1
    }

    fn safevar_index(&self) -> bool {
        codec::version(&self.versions.history) >= 1.2
    }
}
