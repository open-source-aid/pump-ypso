use alloc::boxed::Box;

use pump_shared::{
    Advertisement, BleMatch, Class, CommandKind, Config, Driver, DriverInfo, Engine, Feature,
    GattChar, Kind, Slot, Variant,
};

use crate::engine::YpsoEngine;
use crate::protocol::uuids::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ypso;

pub static DRIVER: Ypso = Ypso;

// Every characteristic the engine may touch is declared below. `notify` is `None`
// throughout because the engine never emits `BleOp::Subscribe`: it polls
// `GetBolusStatus` and `GetSystemStatus` instead, and never addresses
// `CONTROL_NOTIFY` (`Characteristic::BolusStatusNotification` is unused by the
// engine; see `.docs/to-confirm.md` item 1). Rule E9 is checked by
// `the_gatt_table_declares_notify_on_exactly_what_the_engine_subscribes_to`
// in `tests/e2e.rs`; declaring a `notify` here without a matching subscription
// would be just as untruthful as the reverse.
macro_rules! gatt {
    ($name:literal, $service:ident, $characteristic:ident $(,)?) => {
        GattChar {
            name: $name,
            service: $service,
            characteristic: $characteristic,
            notify: None,
        }
    };
}

const VARIANTS: &[Variant] = &[
    Variant {
        id: "ypso",
        name: "YpsoPump",
    },
    #[cfg(feature = "opn")]
    Variant {
        id: "ypso-opn",
        name: "YpsoPump (no CryptoService)",
    },
];

static INFO: DriverInfo = DriverInfo {
    id: "ypso",
    name: "YpsoPump",
    kind: Kind::Pump,
    version: env!("CARGO_PKG_VERSION"),
    api_version: pump_shared::API_VERSION,
    protocol: "ypso-ble",
    ble: BleMatch {
        name_prefixes: &["YpsoPump_"],
        service_uuids: &[],
        manufacturer_id: None,
    },
    gatt: &[
        gatt!(
            "pump base version",
            PUMP_BASE_SERVICE,
            PUMP_BASE_SERVICE_VERSION,
        ),
        gatt!(
            "master software version",
            PUMP_BASE_SERVICE,
            MASTER_SOFTWARE_VERSION,
        ),
        gatt!(
            "authorization password",
            PUMP_BASE_SERVICE,
            AUTHORIZATION_PASSWORD,
        ),
        gatt!(
            "settings version",
            SETTINGS_SERVICE,
            SETTINGS_SERVICE_VERSION,
        ),
        gatt!("setting id", SETTINGS_SERVICE, SETTING_ID),
        gatt!("setting value", SETTINGS_SERVICE, SETTING_VALUE),
        gatt!("system date", SETTINGS_SERVICE, SYSTEM_DATE),
        gatt!("system time", SETTINGS_SERVICE, SYSTEM_TIME),
        gatt!("history version", HISTORY_SERVICE, HISTORY_SERVICE_VERSION),
        gatt!("alarm entry count", HISTORY_SERVICE, ALARM_ENTRY_COUNT),
        gatt!("alarm entry index", HISTORY_SERVICE, ALARM_ENTRY_INDEX),
        gatt!("alarm entry value", HISTORY_SERVICE, ALARM_ENTRY_VALUE),
        gatt!(
            "complaint entry count",
            HISTORY_SERVICE,
            COMPLAINT_ENTRY_COUNT,
        ),
        gatt!(
            "complaint entry index",
            HISTORY_SERVICE,
            COMPLAINT_ENTRY_INDEX,
        ),
        gatt!(
            "complaint entry value",
            HISTORY_SERVICE,
            COMPLAINT_ENTRY_VALUE,
        ),
        gatt!("event entry count", HISTORY_SERVICE, EVENT_ENTRY_COUNT),
        gatt!("event entry index", HISTORY_SERVICE, EVENT_ENTRY_INDEX),
        gatt!("event entry value", HISTORY_SERVICE, EVENT_ENTRY_VALUE),
        gatt!("control version", CONTROL_SERVICE, CONTROL_SERVICE_VERSION),
        gatt!("start stop bolus", CONTROL_SERVICE, START_STOP_BOLUS),
        gatt!("bolus status", CONTROL_SERVICE, GET_BOLUS_STATUS),
        gatt!("start stop temp basal", CONTROL_SERVICE, START_STOP_TBR),
        gatt!("system status", CONTROL_SERVICE, GET_SYSTEM_STATUS),
        gatt!("extended read", CRYPTO_SERVICE, EXTENDED_READ),
    ],
    variants: VARIANTS,
    // `CommandKind::Pair` is absent on purpose. The engine reaches
    // `EngineState::Paired` only by loading a session a host already holds
    // (`engine::load`); `Command::Pair` is refused `Unsupported` and emits no
    // BLE op. The pump half of the exchange exists in `sim::pairing`, but the
    // engine cannot drive it: it issues no `Action::KeyAgree`, `Action::Random`
    // or `Action::Wrap`, and `Characteristic` cannot name `NEW_PUBLIC_KEY` or
    // `WRITE_CHALLENGE_UTC_APP_PUB_KEY`. Declaring `Pair` here would promise a
    // flow that does not exist; see `.docs/pairing.md` for what is missing.
    commands: &[
        CommandKind::Status,
        CommandKind::Unpair,
        #[cfg(feature = "delivery")]
        CommandKind::Bolus,
        #[cfg(feature = "delivery")]
        CommandKind::RateBolus,
        #[cfg(feature = "delivery")]
        CommandKind::TempBasal,
        CommandKind::CancelTempBasal,
        #[cfg(feature = "delivery")]
        CommandKind::ExtendedBolus,
        CommandKind::CancelExtendedBolus,
        CommandKind::ReadClock,
        CommandKind::SyncClock,
        CommandKind::ReadHistory,
        CommandKind::ReadBasalProfile,
        #[cfg(feature = "delivery")]
        CommandKind::SetBasalProfile,
        CommandKind::Stop,
    ],
    features: &[
        Feature {
            name: "delivery",
            enabled: cfg!(feature = "delivery"),
        },
        Feature {
            name: "opn",
            enabled: cfg!(feature = "opn"),
        },
    ],
    slots: &[
        (Slot::Session, Class::Secret),
        (Slot::Counters, Class::Monotonic),
        (Slot::NonceState, Class::Monotonic),
    ],
};

impl Driver for Ypso {
    fn info(&self) -> &'static DriverInfo {
        &INFO
    }

    fn matches(&self, adv: &Advertisement<'_>) -> Option<&'static Variant> {
        INFO.ble.matches(adv).then_some(&VARIANTS[0])
    }

    fn new_engine(&self, variant: &'static Variant, config: Config) -> Box<dyn Engine> {
        Box::new(YpsoEngine::new(variant.id, config))
    }
}
