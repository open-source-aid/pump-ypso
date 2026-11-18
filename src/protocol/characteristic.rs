use pump_shared::Uuid;

use super::uuids::{self, Service};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Characteristic {
    PumpBaseServiceVersion = 0,
    MasterSoftwareVersion = 1,
    SupervisorSoftwareVersion = 2,
    AuthorizationPassword = 3,
    PumpCountryCode = 4,
    SettingsServiceVersion = 5,
    SettingId = 6,
    SettingValue = 7,
    SystemDate = 8,
    SystemTime = 9,
    HistoryServiceVersion = 10,
    AlarmEntryCount = 11,
    AlarmEntryIndex = 12,
    AlarmEntryValue = 13,
    EventEntryCount = 14,
    EventEntryIndex = 15,
    EventEntryValue = 16,
    SystemEntryCount = 17,
    ComplaintEntryCount = 18,
    SystemEntryIndex = 19,
    ComplaintEntryIndex = 20,
    SystemEntryValue = 21,
    ComplaintEntryValue = 22,
    CounterId = 23,
    CounterValue = 24,
    ClearHistoryPassword = 25,
    ControlServiceVersion = 26,
    StartStopBolus = 27,
    GetBolusStatus = 28,
    StartStopTbr = 29,
    GetSystemStatus = 30,
    BolusStatusNotification = 31,
    VirtualChar = 32,
}

impl Characteristic {
    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn uuid(self) -> Option<Uuid> {
        Some(match self {
            Self::PumpBaseServiceVersion => uuids::PUMP_BASE_SERVICE_VERSION,
            Self::MasterSoftwareVersion => uuids::MASTER_SOFTWARE_VERSION,
            Self::SupervisorSoftwareVersion => uuids::SUPERVISOR_SOFTWARE_VERSION,
            Self::AuthorizationPassword => uuids::AUTHORIZATION_PASSWORD,
            Self::SettingsServiceVersion => uuids::SETTINGS_SERVICE_VERSION,
            Self::SettingId => uuids::SETTING_ID,
            Self::SettingValue => uuids::SETTING_VALUE,
            Self::SystemDate => uuids::SYSTEM_DATE,
            Self::SystemTime => uuids::SYSTEM_TIME,
            Self::HistoryServiceVersion => uuids::HISTORY_SERVICE_VERSION,
            Self::AlarmEntryCount => uuids::ALARM_ENTRY_COUNT,
            Self::AlarmEntryIndex => uuids::ALARM_ENTRY_INDEX,
            Self::AlarmEntryValue => uuids::ALARM_ENTRY_VALUE,
            Self::EventEntryCount => uuids::EVENT_ENTRY_COUNT,
            Self::EventEntryIndex => uuids::EVENT_ENTRY_INDEX,
            Self::EventEntryValue => uuids::EVENT_ENTRY_VALUE,
            Self::SystemEntryCount => uuids::SYSTEM_ENTRY_COUNT,
            Self::ComplaintEntryCount => uuids::COMPLAINT_ENTRY_COUNT,
            Self::SystemEntryIndex => uuids::SYSTEM_ENTRY_INDEX,
            Self::ComplaintEntryIndex => uuids::COMPLAINT_ENTRY_INDEX,
            Self::SystemEntryValue => uuids::SYSTEM_ENTRY_VALUE,
            Self::ComplaintEntryValue => uuids::COMPLAINT_ENTRY_VALUE,
            Self::CounterId => uuids::COUNTER_ID,
            Self::CounterValue => uuids::COUNTER_VALUE,
            Self::ControlServiceVersion => uuids::CONTROL_SERVICE_VERSION,
            Self::StartStopBolus => uuids::START_STOP_BOLUS,
            Self::GetBolusStatus => uuids::GET_BOLUS_STATUS,
            Self::StartStopTbr => uuids::START_STOP_TBR,
            Self::GetSystemStatus => uuids::GET_SYSTEM_STATUS,
            Self::BolusStatusNotification => uuids::CONTROL_NOTIFY,
            Self::PumpCountryCode | Self::ClearHistoryPassword | Self::VirtualChar => return None,
        })
    }

    pub fn service(self) -> Option<Service> {
        Some(match self.index() {
            0..=4 => Service::PumpBase,
            5..=9 => Service::Settings,
            10..=25 => Service::History,
            26..=31 => Service::Control,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    const ALL: [Characteristic; 33] = [
        Characteristic::PumpBaseServiceVersion,
        Characteristic::MasterSoftwareVersion,
        Characteristic::SupervisorSoftwareVersion,
        Characteristic::AuthorizationPassword,
        Characteristic::PumpCountryCode,
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
        Characteristic::ClearHistoryPassword,
        Characteristic::ControlServiceVersion,
        Characteristic::StartStopBolus,
        Characteristic::GetBolusStatus,
        Characteristic::StartStopTbr,
        Characteristic::GetSystemStatus,
        Characteristic::BolusStatusNotification,
        Characteristic::VirtualChar,
    ];

    #[test]
    fn indices_are_contiguous_zero_to_thirty_two() {
        for (index, characteristic) in ALL.iter().enumerate() {
            assert_eq!(
                usize::from(characteristic.index()),
                index,
                "{characteristic:?}"
            );
        }
    }

    #[test]
    fn commands_map_to_the_right_service() {
        let services: Vec<Option<Service>> = ALL.iter().map(|c| c.service()).collect();
        let mut expected = vec![Some(Service::PumpBase); 5];
        expected.extend([Some(Service::Settings); 5]);
        expected.extend([Some(Service::History); 16]);
        expected.extend([Some(Service::Control); 6]);
        expected.push(None);
        assert_eq!(services, expected);
    }

    #[test]
    fn every_command_addresses_the_source_characteristic() {
        let expected = [
            Some(uuids::PUMP_BASE_SERVICE_VERSION),
            Some(uuids::MASTER_SOFTWARE_VERSION),
            Some(uuids::SUPERVISOR_SOFTWARE_VERSION),
            Some(uuids::AUTHORIZATION_PASSWORD),
            None,
            Some(uuids::SETTINGS_SERVICE_VERSION),
            Some(uuids::SETTING_ID),
            Some(uuids::SETTING_VALUE),
            Some(uuids::SYSTEM_DATE),
            Some(uuids::SYSTEM_TIME),
            Some(uuids::HISTORY_SERVICE_VERSION),
            Some(uuids::ALARM_ENTRY_COUNT),
            Some(uuids::ALARM_ENTRY_INDEX),
            Some(uuids::ALARM_ENTRY_VALUE),
            Some(uuids::EVENT_ENTRY_COUNT),
            Some(uuids::EVENT_ENTRY_INDEX),
            Some(uuids::EVENT_ENTRY_VALUE),
            Some(uuids::SYSTEM_ENTRY_COUNT),
            Some(uuids::COMPLAINT_ENTRY_COUNT),
            Some(uuids::SYSTEM_ENTRY_INDEX),
            Some(uuids::COMPLAINT_ENTRY_INDEX),
            Some(uuids::SYSTEM_ENTRY_VALUE),
            Some(uuids::COMPLAINT_ENTRY_VALUE),
            Some(uuids::COUNTER_ID),
            Some(uuids::COUNTER_VALUE),
            None,
            Some(uuids::CONTROL_SERVICE_VERSION),
            Some(uuids::START_STOP_BOLUS),
            Some(uuids::GET_BOLUS_STATUS),
            Some(uuids::START_STOP_TBR),
            Some(uuids::GET_SYSTEM_STATUS),
            Some(uuids::CONTROL_NOTIFY),
            None,
        ];
        assert_eq!(ALL.map(Characteristic::uuid), expected);
    }

    #[test]
    fn therapy_commands_are_never_cleartext() {
        for characteristic in [Characteristic::StartStopBolus, Characteristic::StartStopTbr] {
            let uuid = characteristic.uuid().unwrap();
            assert!(!uuids::is_cleartext_read(uuid), "{characteristic:?}");
            assert!(!uuids::is_cleartext_write(uuid), "{characteristic:?}");
        }
    }
}
