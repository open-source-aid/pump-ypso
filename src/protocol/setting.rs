pub const BASAL_PROFILE_SELECTION: u16 = 1;
pub const PUMP_MODE_SELECTION: u16 = 2;
pub const BOLUS_STEP_SELECTION: u16 = 3;
pub const REMAINING_LIFETIME: u16 = 5;
pub const TIME_FORMAT: u16 = 12;
pub const DATE_FORMAT: u16 = 13;

pub const BASAL_PROFILE_A_01: u16 = 14;
pub const BASAL_PROFILE_B_01: u16 = 38;

pub const SERIAL_NUMBER_LOWER: u16 = 62;
pub const SERIAL_NUMBER_UPPER: u16 = 63;
pub const SEQ_LOWER: u16 = 64;
pub const SEQ_UPPER: u16 = 65;
pub const SEQ_HISTORY_ALARMS: u16 = 66;
pub const SEQ_HISTORY_EVENTS: u16 = 67;
pub const AUTO_STOP_TIMEOUT: u16 = 69;

pub const BASAL_RATE_CAP: u16 = 113;
pub const BASAL_RATE_MIN: u16 = 114;
pub const BASAL_RATE_MAX: u16 = 115;

pub const TBR_LONG_SCALE_MAX: u16 = 116;
pub const TBR_LONG_DURATION_MAX: u16 = 117;
pub const TBR_SHORT_SCALE_MAX: u16 = 118;
pub const TBR_SHORT_DURATION_MAX: u16 = 119;
pub const TBR_END_NOT_INDICATED: u16 = 120;

pub const BASAL_SLOTS: u16 = 24;

pub fn basal_a(hour: u16) -> u16 {
    assert!(hour < BASAL_SLOTS, "basal hour {hour} out of range");
    BASAL_PROFILE_A_01 + hour
}

pub fn basal_b(hour: u16) -> u16 {
    assert!(hour < BASAL_SLOTS, "basal hour {hour} out of range");
    BASAL_PROFILE_B_01 + hour
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basal_slots_are_contiguous_and_the_profiles_do_not_overlap() {
        assert_eq!(basal_a(0), 14);
        assert_eq!(basal_a(23), 37);
        assert_eq!(basal_b(0), 38);
        assert_eq!(basal_b(23), 61);
        assert!(basal_b(23) < SERIAL_NUMBER_LOWER);
    }

    #[test]
    #[should_panic(expected = "basal hour 24 out of range")]
    fn profile_a_slot_past_midnight_is_rejected() {
        basal_a(24);
    }

    #[test]
    #[should_panic(expected = "basal hour 24 out of range")]
    fn profile_b_slot_past_midnight_is_rejected() {
        basal_b(24);
    }
}
