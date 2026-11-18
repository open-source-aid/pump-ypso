use pump_shared::Uuid;

const fn uuid(value: u128) -> Uuid {
    Uuid::from_bytes(value.to_be_bytes())
}

pub const PUMP_BASE_SERVICE: Uuid = uuid(0xfb349b5f8000008000100000adde0000);
pub const PUMP_BASE_SERVICE_VERSION: Uuid = uuid(0x669a0c200008969ee211fcbee23b7bc5);
pub const MASTER_SOFTWARE_VERSION: Uuid = uuid(0x669a0c200008969ee211fcbeb0147bc5);
pub const SUPERVISOR_SOFTWARE_VERSION: Uuid = uuid(0x669a0c200008969ee211fcbeb1147bc5);
pub const AUTHORIZATION_PASSWORD: Uuid = uuid(0x669a0c200008969ee211fcbeb2147bc5);
pub const PAIRING_PASSKEY: Uuid = uuid(0x669a0c200008969ee211fcbed63b7bc5);

pub const SETTINGS_SERVICE: Uuid = uuid(0xfb349b5f8000008000100000efbe0000);
pub const STUDY_XX3: Uuid = uuid(0x669a0c200008969ee211fc16889321e6);
pub const STUDY_XX4: Uuid = uuid(0x669a0c200008969ee211fc16889421e6);
pub const STUDY_XX5: Uuid = uuid(0x669a0c200008969ee211fc16889521e6);
pub const SETTING_ID: Uuid = uuid(0x669a0c200008969ee211fcbeb3147bc5);
pub const SETTING_VALUE: Uuid = uuid(0x669a0c200008969ee211fcbeb4147bc5);
pub const SETTING_VALUE_ALT: Uuid = uuid(0x669a0c200008969ee211fcbebd3b7bc5);
pub const SETTING_ID_ALT: Uuid = uuid(0x669a0c200008969ee211fc16889221e6);
pub const SYSTEM_DATE: Uuid = uuid(0x669a0c200008969ee211fcbedc3b7bc5);
pub const SYSTEM_TIME: Uuid = uuid(0x669a0c200008969ee211fcbedd3b7bc5);
pub const SETTINGS_SERVICE_VERSION: Uuid = uuid(0x669a0c200008969ee211fcbee33b7bc5);

pub const HISTORY_SERVICE: Uuid = uuid(0xfb349b5f8000008000100000dec00000);
pub const HISTORY_SERVICE_VERSION: Uuid = uuid(0x669a0c200008969ee211fcbee43b7bc5);
pub const ALARM_ENTRY_COUNT: Uuid = uuid(0x669a0c200008969ee211fcbec83b7bc5);
pub const ALARM_ENTRY_INDEX: Uuid = uuid(0x669a0c200008969ee211fcbec93b7bc5);
pub const ALARM_ENTRY_VALUE: Uuid = uuid(0x669a0c200008969ee211fcbeca3b7bc5);
pub const ALARM_ENTRY_COUNT_ALT: Uuid = uuid(0x669a0c200008969ee211fc1688a021e6);
pub const ALARM_ENTRY_INDEX_ALT: Uuid = uuid(0x669a0c200008969ee211fc1688a121e6);
pub const ALARM_ENTRY_VALUE_ALT: Uuid = uuid(0x669a0c200008969ee211fc1688a221e6);
pub const EVENT_ENTRY_COUNT: Uuid = uuid(0x669a0c200008969ee211fcbecb3b7bc5);
pub const EVENT_ENTRY_INDEX: Uuid = uuid(0x669a0c200008969ee211fcbecc3b7bc5);
pub const EVENT_ENTRY_VALUE: Uuid = uuid(0x669a0c200008969ee211fcbecd3b7bc5);
pub const EVENT_ENTRY_COUNT_ALT: Uuid = uuid(0x669a0c200008969ee211fc16889a21e6);
pub const EVENT_ENTRY_INDEX_ALT: Uuid = uuid(0x669a0c200008969ee211fc16889b21e6);
pub const EVENT_ENTRY_VALUE_ALT: Uuid = uuid(0x669a0c200008969ee211fc16889c21e6);
pub const SYSTEM_ENTRY_COUNT: Uuid = uuid(0x86a5a431d4422c8d304b19ee355571fc);
pub const SYSTEM_ENTRY_INDEX: Uuid = uuid(0x381ddce9e934b4aee345eb87283db426);
pub const SYSTEM_ENTRY_VALUE: Uuid = uuid(0xae3022af2ec8bf88e64cda68c9a3891a);
pub const SYSTEM_ENTRY_COUNT_ALT: Uuid = uuid(0x669a0c200008969ee211fc16889d21e6);
pub const SYSTEM_ENTRY_INDEX_ALT: Uuid = uuid(0x669a0c200008969ee211fc16889e21e6);
pub const SYSTEM_ENTRY_VALUE_ALT: Uuid = uuid(0x669a0c200008969ee211fc16889f21e6);
pub const COMPLAINT_ENTRY_COUNT: Uuid = uuid(0x449753d33cc57594ae4f8d4efa2af4fd);
pub const COMPLAINT_ENTRY_INDEX: Uuid = uuid(0xdb75669be6ccf6a41f4b0becaeb81f34);
pub const COMPLAINT_ENTRY_VALUE: Uuid = uuid(0x813a4e12e1e247a0254422664b3c9e62);
pub const COUNTER_ID: Uuid = uuid(0x669a0c200008969ee211fcbece3b7bc5);
pub const COUNTER_VALUE: Uuid = uuid(0x669a0c200008969ee211fcbecf3b7bc5);
pub const XXX_WRITE: Uuid = uuid(0x669a0c200008969ee211fcbed03b7bc5);

pub const CONTROL_SERVICE: Uuid = uuid(0xfb349b5f8000008000100000feda0000);
pub const CONTROL_SERVICE_VERSION: Uuid = uuid(0x669a0c200008969ee211fcbee08b7bc5);
pub const START_STOP_BOLUS: Uuid = uuid(0x669a0c200008969ee211fcbee18b7bc5);
pub const GET_BOLUS_STATUS: Uuid = uuid(0x669a0c200008969ee211fcbee28b7bc5);
pub const START_STOP_TBR: Uuid = uuid(0x669a0c200008969ee211fcbee38b7bc5);
pub const GET_SYSTEM_STATUS: Uuid = uuid(0x669a0c200008969ee211fcbee48b7bc5);
pub const CONTROL_NOTIFY: Uuid = uuid(0x669a0c200008969ee211fcbee58b7bc5);

pub const CRYPTO_SERVICE: Uuid = uuid(0xfb349b5f8000008000100000feda0002);
pub const CRYPTO_SERVICE_VERSION: Uuid = uuid(0x669a0c200008969ee211fcff0000000c);
pub const NEW_PUBLIC_KEY: Uuid = uuid(0x669a0c200008969ee211fcff0000000a);
pub const WRITE_CHALLENGE_UTC_APP_PUB_KEY: Uuid = uuid(0x669a0c200008969ee211fcff0000000b);
pub const EXTENDED_READ: Uuid = uuid(0x669a0c200008969ee211fcff000000ff);
pub const CCC_DESCRIPTOR: Uuid = uuid(0x0000290200001000800000805f9b34fb);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    PumpBase,
    Settings,
    History,
    Control,
    Crypto,
}

impl Service {
    pub fn uuid(self) -> Uuid {
        match self {
            Self::PumpBase => PUMP_BASE_SERVICE,
            Self::Settings => SETTINGS_SERVICE,
            Self::History => HISTORY_SERVICE,
            Self::Control => CONTROL_SERVICE,
            Self::Crypto => CRYPTO_SERVICE,
        }
    }
}

pub const CLEARTEXT_READ_CHARACTERISTICS: [Uuid; 10] = [
    PUMP_BASE_SERVICE_VERSION,
    MASTER_SOFTWARE_VERSION,
    SUPERVISOR_SOFTWARE_VERSION,
    SETTINGS_SERVICE_VERSION,
    HISTORY_SERVICE_VERSION,
    CONTROL_SERVICE_VERSION,
    CRYPTO_SERVICE_VERSION,
    PAIRING_PASSKEY,
    EXTENDED_READ,
    GET_BOLUS_STATUS,
];

pub const CLEARTEXT_WRITE_CHARACTERISTICS: [Uuid; 2] =
    [AUTHORIZATION_PASSWORD, WRITE_CHALLENGE_UTC_APP_PUB_KEY];

pub fn is_cleartext_read(characteristic: Uuid) -> bool {
    CLEARTEXT_READ_CHARACTERISTICS.contains(&characteristic)
}

pub fn is_cleartext_write(characteristic: Uuid) -> bool {
    CLEARTEXT_WRITE_CHARACTERISTICS.contains(&characteristic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    const SERVICES: [Service; 5] = [
        Service::PumpBase,
        Service::Settings,
        Service::History,
        Service::Control,
        Service::Crypto,
    ];

    fn as_u128(uuid: Uuid) -> u128 {
        u128::from_be_bytes(*uuid.as_bytes())
    }

    #[test]
    fn every_uuid_matches_its_hyphenated_form_from_the_specification() {
        for (uuid, text) in [
            (PUMP_BASE_SERVICE, "fb349b5f-8000-0080-0010-0000adde0000"),
            (
                PUMP_BASE_SERVICE_VERSION,
                "669a0c20-0008-969e-e211-fcbee23b7bc5",
            ),
            (
                MASTER_SOFTWARE_VERSION,
                "669a0c20-0008-969e-e211-fcbeb0147bc5",
            ),
            (
                SUPERVISOR_SOFTWARE_VERSION,
                "669a0c20-0008-969e-e211-fcbeb1147bc5",
            ),
            (
                AUTHORIZATION_PASSWORD,
                "669a0c20-0008-969e-e211-fcbeb2147bc5",
            ),
            (PAIRING_PASSKEY, "669a0c20-0008-969e-e211-fcbed63b7bc5"),
            (SETTINGS_SERVICE, "fb349b5f-8000-0080-0010-0000efbe0000"),
            (STUDY_XX3, "669a0c20-0008-969e-e211-fc16889321e6"),
            (STUDY_XX4, "669a0c20-0008-969e-e211-fc16889421e6"),
            (STUDY_XX5, "669a0c20-0008-969e-e211-fc16889521e6"),
            (SETTING_ID, "669a0c20-0008-969e-e211-fcbeb3147bc5"),
            (SETTING_VALUE, "669a0c20-0008-969e-e211-fcbeb4147bc5"),
            (SETTING_VALUE_ALT, "669a0c20-0008-969e-e211-fcbebd3b7bc5"),
            (SETTING_ID_ALT, "669a0c20-0008-969e-e211-fc16889221e6"),
            (SYSTEM_DATE, "669a0c20-0008-969e-e211-fcbedc3b7bc5"),
            (SYSTEM_TIME, "669a0c20-0008-969e-e211-fcbedd3b7bc5"),
            (
                SETTINGS_SERVICE_VERSION,
                "669a0c20-0008-969e-e211-fcbee33b7bc5",
            ),
            (HISTORY_SERVICE, "fb349b5f-8000-0080-0010-0000dec00000"),
            (
                HISTORY_SERVICE_VERSION,
                "669a0c20-0008-969e-e211-fcbee43b7bc5",
            ),
            (ALARM_ENTRY_COUNT, "669a0c20-0008-969e-e211-fcbec83b7bc5"),
            (ALARM_ENTRY_INDEX, "669a0c20-0008-969e-e211-fcbec93b7bc5"),
            (ALARM_ENTRY_VALUE, "669a0c20-0008-969e-e211-fcbeca3b7bc5"),
            (
                ALARM_ENTRY_COUNT_ALT,
                "669a0c20-0008-969e-e211-fc1688a021e6",
            ),
            (
                ALARM_ENTRY_INDEX_ALT,
                "669a0c20-0008-969e-e211-fc1688a121e6",
            ),
            (
                ALARM_ENTRY_VALUE_ALT,
                "669a0c20-0008-969e-e211-fc1688a221e6",
            ),
            (EVENT_ENTRY_COUNT, "669a0c20-0008-969e-e211-fcbecb3b7bc5"),
            (EVENT_ENTRY_INDEX, "669a0c20-0008-969e-e211-fcbecc3b7bc5"),
            (EVENT_ENTRY_VALUE, "669a0c20-0008-969e-e211-fcbecd3b7bc5"),
            (
                EVENT_ENTRY_COUNT_ALT,
                "669a0c20-0008-969e-e211-fc16889a21e6",
            ),
            (
                EVENT_ENTRY_INDEX_ALT,
                "669a0c20-0008-969e-e211-fc16889b21e6",
            ),
            (
                EVENT_ENTRY_VALUE_ALT,
                "669a0c20-0008-969e-e211-fc16889c21e6",
            ),
            (SYSTEM_ENTRY_COUNT, "86a5a431-d442-2c8d-304b-19ee355571fc"),
            (SYSTEM_ENTRY_INDEX, "381ddce9-e934-b4ae-e345-eb87283db426"),
            (SYSTEM_ENTRY_VALUE, "ae3022af-2ec8-bf88-e64c-da68c9a3891a"),
            (
                SYSTEM_ENTRY_COUNT_ALT,
                "669a0c20-0008-969e-e211-fc16889d21e6",
            ),
            (
                SYSTEM_ENTRY_INDEX_ALT,
                "669a0c20-0008-969e-e211-fc16889e21e6",
            ),
            (
                SYSTEM_ENTRY_VALUE_ALT,
                "669a0c20-0008-969e-e211-fc16889f21e6",
            ),
            (
                COMPLAINT_ENTRY_COUNT,
                "449753d3-3cc5-7594-ae4f-8d4efa2af4fd",
            ),
            (
                COMPLAINT_ENTRY_INDEX,
                "db75669b-e6cc-f6a4-1f4b-0becaeb81f34",
            ),
            (
                COMPLAINT_ENTRY_VALUE,
                "813a4e12-e1e2-47a0-2544-22664b3c9e62",
            ),
            (COUNTER_ID, "669a0c20-0008-969e-e211-fcbece3b7bc5"),
            (COUNTER_VALUE, "669a0c20-0008-969e-e211-fcbecf3b7bc5"),
            (XXX_WRITE, "669a0c20-0008-969e-e211-fcbed03b7bc5"),
            (CONTROL_SERVICE, "fb349b5f-8000-0080-0010-0000feda0000"),
            (
                CONTROL_SERVICE_VERSION,
                "669a0c20-0008-969e-e211-fcbee08b7bc5",
            ),
            (START_STOP_BOLUS, "669a0c20-0008-969e-e211-fcbee18b7bc5"),
            (GET_BOLUS_STATUS, "669a0c20-0008-969e-e211-fcbee28b7bc5"),
            (START_STOP_TBR, "669a0c20-0008-969e-e211-fcbee38b7bc5"),
            (GET_SYSTEM_STATUS, "669a0c20-0008-969e-e211-fcbee48b7bc5"),
            (CONTROL_NOTIFY, "669a0c20-0008-969e-e211-fcbee58b7bc5"),
            (CRYPTO_SERVICE, "fb349b5f-8000-0080-0010-0000feda0002"),
            (
                CRYPTO_SERVICE_VERSION,
                "669a0c20-0008-969e-e211-fcff0000000c",
            ),
            (NEW_PUBLIC_KEY, "669a0c20-0008-969e-e211-fcff0000000a"),
            (
                WRITE_CHALLENGE_UTC_APP_PUB_KEY,
                "669a0c20-0008-969e-e211-fcff0000000b",
            ),
            (EXTENDED_READ, "669a0c20-0008-969e-e211-fcff000000ff"),
            (CCC_DESCRIPTOR, "00002902-0000-1000-8000-00805f9b34fb"),
        ] {
            assert_eq!(uuid.to_string(), text);
        }
    }

    #[test]
    fn service_uuids_share_the_documented_prefix() {
        for service in SERVICES {
            assert_eq!(
                as_u128(service.uuid()) >> 64,
                0xfb34_9b5f_8000_0080,
                "{service:?}"
            );
        }
    }

    #[test]
    fn services_map_to_their_uuids() {
        assert_eq!(
            SERVICES.map(Service::uuid),
            [
                PUMP_BASE_SERVICE,
                SETTINGS_SERVICE,
                HISTORY_SERVICE,
                CONTROL_SERVICE,
                CRYPTO_SERVICE
            ]
        );
    }

    #[test]
    fn uuid_literals_are_read_most_significant_byte_first() {
        assert_eq!(
            uuid(0x00112233_44556677_8899aabb_ccddeeff).as_bytes(),
            &[
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ]
        );
    }

    #[test]
    fn crypto_service_is_distinct_from_control_service() {
        assert_eq!(as_u128(CRYPTO_SERVICE) - as_u128(CONTROL_SERVICE), 2);
    }

    #[test]
    fn is_cleartext_read_covers_every_read_exempt_characteristic() {
        for uuid in [
            PUMP_BASE_SERVICE_VERSION,
            MASTER_SOFTWARE_VERSION,
            SUPERVISOR_SOFTWARE_VERSION,
            SETTINGS_SERVICE_VERSION,
            HISTORY_SERVICE_VERSION,
            CONTROL_SERVICE_VERSION,
            CRYPTO_SERVICE_VERSION,
            PAIRING_PASSKEY,
            EXTENDED_READ,
            GET_BOLUS_STATUS,
        ] {
            assert!(is_cleartext_read(uuid), "{uuid}");
        }
        assert_eq!(CLEARTEXT_READ_CHARACTERISTICS.len(), 10);
    }

    #[test]
    fn is_cleartext_write_covers_exactly_two_characteristics() {
        assert!(is_cleartext_write(AUTHORIZATION_PASSWORD));
        assert!(is_cleartext_write(WRITE_CHALLENGE_UTC_APP_PUB_KEY));
        assert_eq!(CLEARTEXT_WRITE_CHARACTERISTICS.len(), 2);
    }

    #[test]
    fn read_exempt_characteristics_are_not_write_exempt() {
        for uuid in CLEARTEXT_READ_CHARACTERISTICS {
            assert!(!is_cleartext_write(uuid), "{uuid}");
        }
    }

    #[test]
    fn write_exempt_characteristics_are_not_read_exempt() {
        for uuid in CLEARTEXT_WRITE_CHARACTERISTICS {
            assert!(!is_cleartext_read(uuid), "{uuid}");
        }
    }

    #[test]
    fn therapy_and_data_characteristics_are_exempt_from_neither() {
        for uuid in [
            START_STOP_BOLUS,
            START_STOP_TBR,
            GET_SYSTEM_STATUS,
            SETTING_ID,
            SETTING_ID_ALT,
            SETTING_VALUE,
            SETTING_VALUE_ALT,
            SYSTEM_DATE,
            SYSTEM_TIME,
            EVENT_ENTRY_VALUE,
            EVENT_ENTRY_VALUE_ALT,
            ALARM_ENTRY_VALUE,
            COUNTER_VALUE,
            NEW_PUBLIC_KEY,
        ] {
            assert!(!is_cleartext_read(uuid), "{uuid}");
            assert!(!is_cleartext_write(uuid), "{uuid}");
        }
    }

    #[test]
    fn neither_exemption_list_has_duplicates() {
        for list in [
            CLEARTEXT_READ_CHARACTERISTICS.to_vec(),
            CLEARTEXT_WRITE_CHARACTERISTICS.to_vec(),
        ] {
            let mut seen: Vec<Uuid> = list.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), list.len());
        }
    }

    #[test]
    fn alt_variants_differ_from_their_base_characteristics() {
        for (base, alt) in [
            (SETTING_ID, SETTING_ID_ALT),
            (SETTING_VALUE, SETTING_VALUE_ALT),
            (EVENT_ENTRY_COUNT, EVENT_ENTRY_COUNT_ALT),
            (EVENT_ENTRY_INDEX, EVENT_ENTRY_INDEX_ALT),
            (EVENT_ENTRY_VALUE, EVENT_ENTRY_VALUE_ALT),
            (ALARM_ENTRY_COUNT, ALARM_ENTRY_COUNT_ALT),
            (ALARM_ENTRY_INDEX, ALARM_ENTRY_INDEX_ALT),
            (ALARM_ENTRY_VALUE, ALARM_ENTRY_VALUE_ALT),
            (SYSTEM_ENTRY_COUNT, SYSTEM_ENTRY_COUNT_ALT),
            (SYSTEM_ENTRY_INDEX, SYSTEM_ENTRY_INDEX_ALT),
            (SYSTEM_ENTRY_VALUE, SYSTEM_ENTRY_VALUE_ALT),
        ] {
            assert_ne!(base, alt);
        }
    }
}
