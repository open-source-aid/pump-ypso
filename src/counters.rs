use core::fmt;

pub const UNSEEDED: i64 = -1;

const FIRST_STEP: u64 = 16;
const MAX_STEP: u64 = 1 << 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EpochChange {
    Same,
    CountingRestarted,
    CountingCarriedOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CounterState {
    Unseeded,
    Seeded(u64),
}

impl CounterState {
    fn from_session(value: i64) -> Self {
        u64::try_from(value).map_or(Self::Unseeded, Self::Seeded)
    }

    fn to_session(self) -> i64 {
        match self {
            Self::Seeded(value) => value as i64,
            Self::Unseeded => UNSEEDED,
        }
    }

    fn raised_to(self, counter: u64) -> Self {
        match self {
            Self::Seeded(current) => Self::Seeded(current.max(counter)),
            Self::Unseeded => Self::Seeded(counter),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CounterError {
    Unseeded,
    Exhausted,
}

impl fmt::Display for CounterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unseeded => "write counter is unseeded; seed it with a read before writing",
            Self::Exhausted => "counter resynchronisation gave up after the maximum forward jump",
        })
    }
}

impl core::error::Error for CounterError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Counters {
    write: CounterState,
    read: CounterState,
    step: u64,
    epoch: u32,
}

impl Counters {
    pub fn from_session(write: i64, read: i64, reboot: u32) -> Self {
        Self {
            write: CounterState::from_session(write),
            read: CounterState::from_session(read),
            step: FIRST_STEP,
            epoch: reboot,
        }
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn write_state(&self) -> CounterState {
        self.write
    }

    pub fn read_state(&self) -> CounterState {
        self.read
    }

    pub fn observe_reboot(&mut self, pump_reboot: u32, pump_counter: u64) -> EpochChange {
        if pump_reboot <= self.epoch {
            return EpochChange::Same;
        }
        self.epoch = pump_reboot;
        self.step = FIRST_STEP;
        if matches!(self.read, CounterState::Seeded(seen) if pump_counter > seen) {
            return EpochChange::CountingCarriedOn;
        }
        self.write = CounterState::Seeded(0);
        self.read = CounterState::Unseeded;
        EpochChange::CountingRestarted
    }

    pub fn read_counter_acceptable(&self, pump_counter: u64) -> bool {
        match self.read {
            CounterState::Unseeded => true,
            CounterState::Seeded(seen) => pump_counter > seen,
        }
    }

    pub fn next_write(&self) -> Result<u64, CounterError> {
        match self.write {
            CounterState::Seeded(value) => Ok(value.saturating_add(1)),
            CounterState::Unseeded => Err(CounterError::Unseeded),
        }
    }

    pub fn accept_write(&mut self, counter: u64) {
        self.write = self.write.raised_to(counter);
        self.step = FIRST_STEP;
    }

    pub fn accept_read(&mut self, counter: u64) {
        self.read = self.read.raised_to(counter);
    }

    pub fn on_counter_error(&mut self) -> Result<u64, CounterError> {
        if self.step > MAX_STEP {
            return Err(CounterError::Exhausted);
        }
        let base = match self.write {
            CounterState::Seeded(value) => value,
            CounterState::Unseeded => 0,
        };
        let candidate = base.saturating_add(self.step);
        self.write = CounterState::Seeded(candidate);
        self.step = self.step.saturating_mul(2);
        Ok(candidate)
    }

    pub fn as_session_pair(&self) -> (i64, i64) {
        (self.write.to_session(), self.read.to_session())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn minus_one_is_unseeded_not_zero() {
        let counters = Counters::from_session(UNSEEDED, UNSEEDED, 0);
        assert_eq!(counters.write_state(), CounterState::Unseeded);
        assert_eq!(counters.read_state(), CounterState::Unseeded);
        assert_eq!(counters.next_write(), Err(CounterError::Unseeded));
    }

    #[test]
    fn any_negative_session_value_is_unseeded() {
        let counters = Counters::from_session(-7, i64::MIN, 0);
        assert_eq!(counters.write_state(), CounterState::Unseeded);
        assert_eq!(counters.read_state(), CounterState::Unseeded);
        assert_eq!(counters.as_session_pair(), (UNSEEDED, UNSEEDED));
    }

    #[test]
    fn zero_is_a_real_counter() {
        let counters = Counters::from_session(0, 0, 0);
        assert_eq!(counters.write_state(), CounterState::Seeded(0));
        assert_eq!(counters.read_state(), CounterState::Seeded(0));
        assert_eq!(counters.next_write(), Ok(1));
    }

    #[test]
    fn seeded_counter_increments() {
        let mut counters = Counters::from_session(41, 0, 0);
        assert_eq!(counters.next_write(), Ok(42));
        counters.accept_write(42);
        assert_eq!(counters.next_write(), Ok(43));
    }

    #[test]
    fn next_write_saturates_instead_of_wrapping() {
        let mut counters = Counters::from_session(0, 0, 0);
        counters.accept_write(u64::MAX);
        assert_eq!(counters.next_write(), Ok(u64::MAX));
        assert_eq!(counters.on_counter_error(), Ok(u64::MAX));
    }

    #[test]
    fn counter_never_moves_backwards() {
        let mut counters = Counters::from_session(100, 0, 0);
        counters.accept_write(50);
        assert_eq!(counters.write_state(), CounterState::Seeded(100));
    }

    #[test]
    fn an_accepted_write_seeds_an_unseeded_counter() {
        let mut counters = Counters::from_session(UNSEEDED, UNSEEDED, 0);
        counters.accept_write(30);
        assert_eq!(counters.write_state(), CounterState::Seeded(30));
    }

    #[test]
    fn forward_jump_doubles_on_consecutive_misses() {
        let mut counters = Counters::from_session(0, 0, 0);
        assert_eq!(counters.on_counter_error(), Ok(16));
        assert_eq!(counters.on_counter_error(), Ok(16 + 32));
        assert_eq!(counters.on_counter_error(), Ok(16 + 32 + 64));
    }

    #[test]
    fn accepting_a_write_resets_the_step() {
        let mut counters = Counters::from_session(0, 0, 0);
        counters.on_counter_error().unwrap();
        counters.on_counter_error().unwrap();
        counters.accept_write(500);
        assert_eq!(counters.on_counter_error(), Ok(500 + FIRST_STEP));
    }

    #[test]
    fn unseeded_counter_can_be_seeded_by_scanning() {
        let mut counters = Counters::from_session(UNSEEDED, UNSEEDED, 0);
        assert_eq!(counters.on_counter_error(), Ok(FIRST_STEP));
        assert_eq!(counters.write_state(), CounterState::Seeded(FIRST_STEP));
    }

    #[test]
    fn resynchronisation_gives_up_after_sixteen_jumps() {
        let mut counters = Counters::from_session(0, 0, 0);
        let mut last = 0;
        for _ in 0..16 {
            last = counters.on_counter_error().unwrap();
        }
        assert_eq!(last, (1 << 20) - FIRST_STEP);
        assert_eq!(counters.on_counter_error(), Err(CounterError::Exhausted));
        assert_eq!(counters.write_state(), CounterState::Seeded(last));
    }

    #[test]
    fn a_pump_reboot_resets_both_counters_not_just_the_write() {
        let mut counters = Counters::from_session(900, 800, 3);
        counters.accept_read(800);
        assert_eq!(
            counters.observe_reboot(4, 2),
            EpochChange::CountingRestarted
        );
        assert!(counters.read_counter_acceptable(1));
        assert_eq!(counters.write_state(), CounterState::Seeded(0));
        assert_eq!(counters.read_state(), CounterState::Unseeded);
        assert_eq!(counters.epoch(), 4);
    }

    #[test]
    fn a_new_epoch_with_an_equal_reply_counter_restarts_the_counting() {
        let mut counters = Counters::from_session(900, 800, 3);
        assert_eq!(
            counters.observe_reboot(4, 800),
            EpochChange::CountingRestarted
        );
    }

    #[test]
    fn a_new_epoch_with_no_read_counter_restarts_the_counting() {
        let mut counters = Counters::from_session(900, UNSEEDED, 3);
        assert_eq!(
            counters.observe_reboot(4, 5000),
            EpochChange::CountingRestarted
        );
        assert_eq!(counters.write_state(), CounterState::Seeded(0));
    }

    #[test]
    fn a_new_epoch_resets_the_step() {
        let mut counters = Counters::from_session(2045, 556, 9);
        counters.on_counter_error().unwrap();
        counters.on_counter_error().unwrap();
        assert_eq!(
            counters.observe_reboot(10, 559),
            EpochChange::CountingCarriedOn
        );
        assert_eq!(counters.on_counter_error(), Ok(2045 + 16 + 32 + FIRST_STEP));
    }

    #[test]
    fn a_new_epoch_whose_counting_carried_on_keeps_the_counters() {
        let mut counters = Counters::from_session(2045, 556, 9);
        counters.accept_read(556);
        assert_eq!(
            counters.observe_reboot(10, 559),
            EpochChange::CountingCarriedOn
        );
        assert_eq!(counters.epoch(), 10);
        assert_eq!(counters.write_state(), CounterState::Seeded(2045));
        assert_eq!(counters.next_write(), Ok(2046));
        counters.accept_read(559);
        assert!(!counters.read_counter_acceptable(558));
    }

    #[test]
    fn a_stale_read_counter_is_still_rejected_within_an_epoch() {
        let mut counters = Counters::from_session(0, 0, 3);
        counters.accept_read(500);
        counters.accept_read(400);
        assert!(!counters.read_counter_acceptable(499));
        assert!(!counters.read_counter_acceptable(500));
        assert!(counters.read_counter_acceptable(501));
    }

    #[test]
    fn an_accepted_read_seeds_an_unseeded_read_counter() {
        let mut counters = Counters::from_session(0, UNSEEDED, 3);
        counters.accept_read(12);
        assert_eq!(counters.read_state(), CounterState::Seeded(12));
    }

    #[test]
    fn the_same_or_an_older_reboot_counter_is_not_a_new_epoch() {
        let mut counters = Counters::from_session(900, 800, 7);
        counters.accept_read(800);
        assert_eq!(counters.observe_reboot(7, 801), EpochChange::Same);
        assert_eq!(counters.observe_reboot(6, 1), EpochChange::Same);
        assert!(!counters.read_counter_acceptable(1));
        assert_eq!(counters.write_state(), CounterState::Seeded(900));
        assert_eq!(counters.epoch(), 7);
    }

    #[test]
    fn session_pair_round_trips_the_sentinel() {
        let counters = Counters::from_session(UNSEEDED, 7, 0);
        assert_eq!(counters.as_session_pair(), (UNSEEDED, 7));
    }

    #[test]
    fn errors_display_their_meaning() {
        assert_eq!(
            CounterError::Unseeded.to_string(),
            "write counter is unseeded; seed it with a read before writing"
        );
        assert_eq!(
            CounterError::Exhausted.to_string(),
            "counter resynchronisation gave up after the maximum forward jump"
        );
    }

    #[test]
    fn a_large_gap_is_closed_by_the_scan_over_a_few_commands() {
        let pump: u64 = 1_000 + (1 << 22) + 5;
        let mut persisted: i64 = 1_000;
        let mut commands = 0;
        let caught = loop {
            commands += 1;
            assert!(commands <= 8, "not caught up in 8 commands");
            let mut counters = Counters::from_session(persisted, 0, 3);
            let mut sent = counters.next_write().unwrap();
            let mut done = false;
            for _ in 0..17 {
                if sent > pump {
                    counters.accept_write(sent);
                    done = true;
                    break;
                }
                match counters.on_counter_error() {
                    Ok(next) => sent = next,
                    Err(_) => break,
                }
            }
            persisted = counters.as_session_pair().0;
            if done {
                break sent;
            }
        };
        assert!(caught > pump);
        assert!(commands <= 5, "{commands} commands");
        assert!(caught - pump <= 1 << 20);
    }
}
