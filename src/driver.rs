use alloc::boxed::Box;

use core::time::Duration;

use pump_shared::{
    Address, Advertisement, BasalRate, BleMatch, Class, CommandKind, Config, Driver, DriverInfo,
    DriverMatch, Engine, Feature, GattChar, InsulinAmount, Kind, Nominal, NominalBounds,
    NominalCapabilities, NominalLimits, Percent, Slot, Tier, Variant,
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

// Neither variant overrides `NOMINAL`. `ypso-opn` is the same pump: it differs
// only in whether the firmware exposes `CRYPTO_SERVICE` (`sim::opn`,
// `link::keyless_ok`), which changes how the link is secured and nothing about
// what the mechanism can deliver. A model with a different cartridge or a
// different ceiling would carry its own `Nominal` here.
const VARIANTS: &[Variant] = &[
    Variant {
        id: "ypso",
        name: "YpsoPump",
        nominal: None,
    },
    #[cfg(feature = "opn")]
    Variant {
        id: "ypso-opn",
        name: "YpsoPump (no CryptoService)",
        nominal: None,
    },
];

const fn amount(milliunits: u32) -> InsulinAmount {
    InsulinAmount::from_milliunits(milliunits)
}

const fn rate(milliunits_per_hour: u32) -> BasalRate {
    BasalRate::from_milliunits_per_hour(milliunits_per_hour)
}

const MINUTE: Duration = Duration::from_secs(60);
const QUARTER: Duration = Duration::from_secs(15 * 60);

/// What a YpsoPump is built for, before one is connected — for a setup
/// screen, never for checking a dose. What a *given* pump accepts is narrower
/// and arrives with `DeviceInfo`: the bolus step (setting 3) and the maximum
/// bolus (setting 112) are the user's, the basal floor and ceiling are
/// settings 113..=115, and the native extended bolus exists only above
/// control-service 1.1.
///
/// Every figure below is one this driver already relies on elsewhere; the
/// source is named. Nothing is rounded up from a guess, and what is not
/// evidenced anywhere is left absent:
///
/// * **Delivery speed** — no field here carries it, and the only numbers we
///   have (`sim`'s 87 ms per hundredth; `.docs/extended-bolus-emulation.md`'s
///   peak ~12 U/min) are marked TO CONFIRM on the bench.
/// * **Daily totals** — the pump logs them (event 23), but this driver does
///   not read them, so it claims no more than `engine::pump::capabilities()`.
static NOMINAL: Nominal = Nominal {
    limits: NominalLimits {
        // 0.05 U is the finest step the pump offers: setting 3 is in
        // hundredths, a multiple of 5, capped at 100 (`Ctx::device`), so a
        // given pump's step is 0.05..1.00 U and the family grid is the 0.05 U
        // one. The floor is the smallest total the pump will take, 0.10 U
        // (`sim::therapy::MIN_BOLUS`), which is above one step: the stricter
        // of the two is the honest one. The ceiling is the largest value
        // setting 112 can hold, 25 U (`engine::pump::Device::new`).
        bolus: Some(NominalBounds {
            min: amount(100),
            max: amount(25_000),
            tiers: &[Tier {
                from: InsulinAmount::ZERO,
                step: amount(50),
            }],
        }),
        // The tier table of `engine::pump::Device::new`, with the floor and
        // ceiling of `Device::defaults` (0.02 U/h, 40 U/h), which the
        // simulator's settings 114/115 agree with.
        basal: Some(NominalBounds {
            min: rate(20),
            max: rate(40_000),
            tiers: &[
                Tier {
                    from: rate(0),
                    step: rate(10),
                },
                Tier {
                    from: rate(1_000),
                    step: rate(20),
                },
                Tier {
                    from: rate(2_000),
                    step: rate(100),
                },
                Tier {
                    from: rate(15_000),
                    step: rate(500),
                },
            ],
        }),
        // Settings 116/118 cap the scale at 200 % on every pump we have seen,
        // and the engine builds the percent bounds with a 1 % step.
        temp_basal_percent: Some(NominalBounds {
            min: Percent::new(0),
            max: Percent::new(200),
            tiers: &[Tier {
                from: Percent::new(0),
                step: Percent::new(1),
            }],
        }),
        // The pump has no absolute temp basal at all; the engine reaches one
        // by other means (`.docs/extended-bolus-emulation.md`).
        temp_basal_absolute: None,
        // 15 minutes to 4 hours in 15-minute steps: `QUARTER` in
        // `Device::new`, settings 117/119.
        temp_basal_duration: Some(NominalBounds {
            min: QUARTER,
            max: Duration::from_secs(240 * 60),
            tiers: &[Tier {
                from: Duration::ZERO,
                step: QUARTER,
            }],
        }),
        // The pump's own extended bolus, 15 minutes to 12 hours by the minute
        // (`Device::new`, `sim::therapy::SLOW_MINUTES`). Firmware at or below
        // control-service 1.1 has none, which only the device can say.
        extended_bolus_duration: Some(NominalBounds {
            min: QUARTER,
            max: Duration::from_secs(720 * 60),
            tiers: &[Tier {
                from: Duration::ZERO,
                step: MINUTE,
            }],
        }),
    },
    capabilities: NominalCapabilities {
        // The three figures `engine::pump::capabilities()` reports for every
        // connected pump: a 1.6 mL cartridge at U100, and the 24 hourly slots
        // of `setting::BASAL_SLOTS`.
        reservoir_capacity: Some(amount(160_000)),
        max_profile_segments: Some(24),
        min_segment_duration: Some(Duration::from_secs(3600)),
        history: true,
        daily_totals: false,
        patch: false,
    },
};

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
        gatt!("pairing public key", CRYPTO_SERVICE, NEW_PUBLIC_KEY),
        gatt!(
            "pairing challenge",
            CRYPTO_SERVICE,
            WRITE_CHALLENGE_UTC_APP_PUB_KEY,
        ),
    ],
    variants: VARIANTS,
    // The engine reaches `EngineState::Paired` either by loading a session a
    // host already holds (`engine::load`) or by running the offline pairing
    // exchange (`engine::link::offline_pair`), which is what `Pair` drives.
    commands: &[
        CommandKind::Status,
        CommandKind::Pair,
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
    nominal: NOMINAL,
};

impl Driver for Ypso {
    fn info(&self) -> &'static DriverInfo {
        &INFO
    }

    fn matches(&self, adv: &Advertisement<'_>) -> Option<DriverMatch> {
        if !INFO.ble.matches(adv) {
            return None;
        }
        // The advertised name is `YpsoPump_<serial>`; the serial yields the MAC that a platform like
        // iOS never exposes, so the harness can bind on it. A non-numeric or out-of-range suffix
        // leaves the address underived.
        let address = adv
            .name
            .and_then(|name| name.strip_prefix("YpsoPump_"))
            .and_then(|serial| serial.parse::<u32>().ok())
            .and_then(crate::auth::mac_from_serial)
            .map(Address::new);
        Some(DriverMatch {
            driver_id: INFO.id,
            variant: &VARIANTS[0],
            address,
        })
    }

    fn new_engine(&self, variant: &'static Variant, config: Config) -> Box<dyn Engine> {
        Box::new(YpsoEngine::new(variant.id, config))
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    fn family() -> &'static Nominal {
        DRIVER.info().nominal_for(&VARIANTS[0])
    }

    #[test]
    fn every_model_shares_the_family_declaration() {
        // Same pump, different link security; see the comment on `VARIANTS`.
        for variant in VARIANTS {
            assert_eq!(variant.nominal, None);
            assert_eq!(DRIVER.info().nominal_for(variant), &NOMINAL);
        }
    }

    #[test]
    fn matches_derives_the_binding_address_from_the_advertised_serial() {
        fn advert(name: Option<&str>) -> Advertisement<'_> {
            Advertisement {
                address: Address::new([0; 6]),
                name,
                service_uuids: &[],
                manufacturer: None,
                rssi: -50,
            }
        }
        // A YpsoPump advert → the variant plus the MAC derived from the serial in the name.
        let m = DRIVER.matches(&advert(Some("YpsoPump_10155555"))).unwrap();
        assert_eq!(m.driver_id, INFO.id);
        assert_eq!(m.variant.id, VARIANTS[0].id);
        assert_eq!(
            m.address,
            crate::auth::mac_from_serial(10_155_555).map(Address::new)
        );
        assert!(m.address.is_some());
        // Matched, but the suffix is not a serial → no derived address.
        assert_eq!(
            DRIVER.matches(&advert(Some("YpsoPump_x"))).unwrap().address,
            None
        );
        // Matched, but the serial is out of MAC range → no derived address.
        assert_eq!(
            DRIVER
                .matches(&advert(Some("YpsoPump_999999999")))
                .unwrap()
                .address,
            None
        );
        // A foreign advert, and one with no name → no match.
        assert!(DRIVER.matches(&advert(Some("Other"))).is_none());
        assert!(DRIVER.matches(&advert(None)).is_none());
    }

    #[test]
    fn the_nominal_bolus_grid_is_the_finest_step_the_pump_offers() {
        let bolus = family().limits.bolus.unwrap();
        let values = bolus.values();
        // 0.10 U up to 25 U, every 0.05 U.
        assert_eq!(values.first(), Some(&amount(100)));
        assert_eq!(values.last(), Some(&amount(25_000)));
        assert_eq!(values.len(), 499);
        assert!(values
            .windows(2)
            .all(|w| w[1].milliunits() - w[0].milliunits() == 50));
    }

    #[test]
    fn the_nominal_basal_grid_carries_every_tier_the_pump_switches_at() {
        let basal = family().limits.basal.unwrap();
        let values = basal.values();
        assert_eq!(values.first(), Some(&rate(20)));
        assert_eq!(values.last(), Some(&rate(40_000)));
        // 0.01 U/h to 1 U/h, then 0.02, then 0.1, then 0.5 from 15 U/h.
        let steps: Vec<u32> = basal
            .tiers
            .iter()
            .map(|t| t.step.milliunits_per_hour())
            .collect();
        assert_eq!(steps, [10, 20, 100, 500]);
        assert!(values.contains(&rate(990)));
        assert!(!values.contains(&rate(1_010)));
        assert!(values.contains(&rate(2_100)));
        assert!(values.contains(&rate(15_500)));
        assert!(!values.contains(&rate(15_100)));
    }

    #[test]
    fn the_nominal_temp_basal_is_percent_only_and_capped_at_four_hours() {
        let limits = &family().limits;
        assert_eq!(limits.temp_basal_absolute, None);
        let percent = limits.temp_basal_percent.unwrap();
        assert_eq!(percent.min, Percent::new(0));
        assert_eq!(percent.max, Percent::new(200));
        assert_eq!(percent.values().len(), 201);
        let duration = limits.temp_basal_duration.unwrap();
        assert_eq!(duration.min, QUARTER);
        assert_eq!(duration.max, Duration::from_secs(4 * 3600));
        assert_eq!(duration.values().len(), 16);
    }

    #[test]
    fn the_nominal_extended_bolus_runs_by_the_minute_up_to_twelve_hours() {
        let extended = family().limits.extended_bolus_duration.unwrap();
        assert_eq!(extended.min, QUARTER);
        assert_eq!(extended.max, Duration::from_secs(12 * 3600));
        let minutes: Vec<u64> = extended.values().iter().map(Duration::as_secs).collect();
        assert_eq!(minutes.len(), 720 - 15 + 1);
        assert_eq!(minutes[1] - minutes[0], 60);
    }

    #[test]
    fn the_nominal_capabilities_are_the_cartridge_and_the_hourly_slots() {
        let capabilities = family().capabilities;
        assert_eq!(capabilities.reservoir_capacity, Some(amount(160_000)));
        assert_eq!(
            capabilities.max_profile_segments,
            Some(crate::protocol::setting::BASAL_SLOTS as u8)
        );
        assert_eq!(
            capabilities.min_segment_duration,
            Some(Duration::from_secs(3600))
        );
        assert!(capabilities.history);
        // The pump logs daily totals, but this driver does not read them.
        assert!(!capabilities.daily_totals);
        assert!(!capabilities.patch);
    }

    #[test]
    fn the_declaration_is_nominal_and_the_driver_is_still_compatible() {
        let info = DRIVER.info();
        assert!(info.is_compatible());
        // Nothing here is a device reading: a pump's own `Limits` arrive in
        // `Event::DeviceInfo` and are the only thing a dose is checked
        // against. The two cannot be mixed up: `NominalBounds` has no
        // `allows`, no `floor` and no `nearest`, and does not convert to
        // `Bounds`.
        assert_eq!(info.nominal, NOMINAL);
    }
}
