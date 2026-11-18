const HUNDREDTHS_PER_UNIT: f64 = 100.0;

pub fn hundredths_to_units(hundredths: i64) -> f64 {
    hundredths as f64 / HUNDREDTHS_PER_UNIT
}

pub fn units_to_hundredths(units: f64) -> i64 {
    libm::round(units * HUNDREDTHS_PER_UNIT) as i64
}

pub fn assemble_serial(upper: u16, lower: u16) -> u32 {
    (u32::from(upper) << 16) | u32::from(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hundredths_match_the_handover_examples() {
        assert!((hundredths_to_units(139) - 1.39).abs() < 1e-9);
        assert!((hundredths_to_units(20) - 0.20).abs() < 1e-9);
        assert_eq!(units_to_hundredths(1.39), 139);
        assert_eq!(units_to_hundredths(0.20), 20);
    }

    #[test]
    fn hundredths_round_trip() {
        for hundredths in [0i64, 1, 5, 20, 139, 500, 1000, 3000, -139] {
            assert_eq!(
                units_to_hundredths(hundredths_to_units(hundredths)),
                hundredths
            );
        }
    }

    #[test]
    fn units_to_hundredths_rounds_half_away_from_zero() {
        assert_eq!(units_to_hundredths(0.005), 1);
        assert_eq!(units_to_hundredths(0.004), 0);
        assert_eq!(units_to_hundredths(1.005), 100);
        assert_eq!(units_to_hundredths(-1.39), -139);
        assert_eq!(units_to_hundredths(0.125), 13);
        assert_eq!(units_to_hundredths(-0.125), -13);
    }

    #[test]
    fn serial_matches_the_handover_worked_example() {
        assert_eq!(assemble_serial(154, 63011), 10_155_555);
    }

    #[test]
    fn signed_misread_of_the_lower_half_produces_the_wrong_serial() {
        let upper: u16 = 154;
        let lower: u16 = 63011;
        let misread = lower as i16;
        assert_eq!(misread, -2525);
        let naive = (i32::from(upper) << 16) | i32::from(misread);
        assert_eq!(naive, -2525);
        assert_eq!(assemble_serial(upper, lower), 10_155_555);
    }

    #[test]
    fn swapping_the_halves_gives_a_different_serial() {
        assert_ne!(assemble_serial(154, 63011), assemble_serial(63011, 154));
    }

    #[test]
    fn serial_handles_both_extremes() {
        assert_eq!(assemble_serial(0, 0), 0);
        assert_eq!(assemble_serial(0, 63011), 63011);
        assert_eq!(assemble_serial(u16::MAX, u16::MAX), u32::MAX);
        assert_eq!(assemble_serial(1, 0), 65536);
    }
}
