use alloc::vec::Vec;
use core::time::Duration;

use pump_shared::{
    Alarm, AlarmKind, BasalRate, BolusKind, Component, Failure, HistoryEntry, HistoryEvent,
    InsulinAmount, LimitChange, Percent, PumpTime, Sequence, TempBasalPhase, TempBasalRate,
};

use super::crc::crc16_ok;
use super::link::{safevar, Ctx, Halt, Step, WriteClass};
use super::pump::pump_seconds;
use crate::Characteristic;

const ENTRY_LEN: usize = 19;
const VALUE_INVALID: u8 = 0x82;
const INDEX_INVALID: u8 = 0x84;
const LOG_SHIFT: u32 = 30;
const SEQUENCE_MASK: u32 = (1 << LOG_SHIFT) - 1;
const DEFAULT_BOLUS_CAP: u32 = 3_000;
const DEFAULT_BASAL_CAP: u32 = 4_000;

struct Raw {
    seconds: u32,
    kind: u8,
    values: [u16; 3],
    sequence: u32,
    index: u16,
    bytes: Vec<u8>,
}

impl Raw {
    fn pump_time(&self) -> PumpTime {
        PumpTime::from_local_seconds(pump_seconds(self.seconds))
    }
}

#[derive(Clone, Copy)]
enum Log {
    Alarm = 1,
    Complaint = 2,
    Event = 3,
}

impl Log {
    fn characteristics(self) -> [Characteristic; 3] {
        match self {
            Log::Alarm => [
                Characteristic::AlarmEntryCount,
                Characteristic::AlarmEntryIndex,
                Characteristic::AlarmEntryValue,
            ],
            Log::Complaint => [
                Characteristic::ComplaintEntryCount,
                Characteristic::ComplaintEntryIndex,
                Characteristic::ComplaintEntryValue,
            ],
            Log::Event => [
                Characteristic::EventEntryCount,
                Characteristic::EventEntryIndex,
                Characteristic::EventEntryValue,
            ],
        }
    }

    fn decode(self, raw: &Raw) -> Option<HistoryEvent> {
        match self {
            Log::Alarm => alarm(raw),
            Log::Complaint => complaint(raw),
            Log::Event => event(raw),
        }
    }

    fn sequence(self, raw: &Raw) -> Sequence {
        Sequence::new((self as u32) << LOG_SHIFT | raw.sequence & SEQUENCE_MASK)
    }
}

fn parse(bytes: &[u8]) -> Option<Raw> {
    let le16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let le32 =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    (bytes.len() == ENTRY_LEN && crc16_ok(bytes)).then(|| Raw {
        seconds: le32(0),
        kind: bytes[4],
        values: [le16(5), le16(7), le16(9)],
        sequence: le32(11),
        index: le16(15),
        bytes: bytes.to_vec(),
    })
}

fn event(raw: &Raw) -> Option<HistoryEvent> {
    let [a, b, c] = raw.values;
    let amount = |v: u16| InsulinAmount::from_milliunits(u32::from(v) * 10);
    let cap = |v: u16, default: u32| u32::try_from(v as i16).unwrap_or(default) * 10;
    let minutes = |v: u16| Duration::from_secs(u64::from(v) * 60);
    let bolus = |kind, requested: u16, delivered: u16, duration, in_progress| HistoryEvent::Bolus {
        kind,
        requested: amount(requested),
        delivered: amount(delivered),
        duration,
        automatic: false,
        in_progress,
    };
    let temp_basal = |percent: u16, duration, phase| HistoryEvent::TempBasal {
        rate: TempBasalRate::Percent(Percent::new(percent)),
        duration,
        phase,
    };
    Some(match raw.kind {
        2 | 26 | 28 | 29 => bolus(BolusKind::Normal, a, a, Duration::ZERO, false),
        19 | 27 => bolus(BolusKind::Normal, a, 0, Duration::ZERO, true),
        3 | 30 => bolus(BolusKind::Extended, a, a, minutes(b), false),
        1 => bolus(BolusKind::Extended, a, 0, minutes(b), true),
        18 | 31 => bolus(BolusKind::Combined, a, a, minutes(c), false),
        17 => bolus(BolusKind::Combined, a, 0, minutes(c), true),
        9 => temp_basal(a, minutes(b), TempBasalPhase::Started),
        10 | 32 => temp_basal(a, minutes(b), TempBasalPhase::Ended),
        12 | 13 => HistoryEvent::ClockChanged {
            previous: raw.pump_time(),
        },
        6..=8 => HistoryEvent::BasalProfileChanged,
        14 if a == 10 => HistoryEvent::Resumed,
        14 => HistoryEvent::Suspended,
        16 => HistoryEvent::Rewound,
        24 => HistoryEvent::Replaced(Component::Battery),
        25 => HistoryEvent::Replaced(Component::Cannula),
        4 => bolus(BolusKind::Priming, a, a, Duration::ZERO, false),
        23 => HistoryEvent::DailyTotal {
            day: raw.pump_time(),
            amount: InsulinAmount::from_milliunits(
                (u32::from(b) << 16 | u32::from(a)).saturating_mul(10),
            ),
        },
        5 if a % 5 == 0 && (5..=100).contains(&a) => {
            HistoryEvent::LimitChanged(LimitChange::BolusStep { value: amount(a) })
        }
        33 => HistoryEvent::LimitChanged(LimitChange::MaxBolus {
            value: InsulinAmount::from_milliunits(cap(a, DEFAULT_BOLUS_CAP)),
            previous: InsulinAmount::from_milliunits(cap(b, DEFAULT_BOLUS_CAP)),
        }),
        34 => HistoryEvent::LimitChanged(LimitChange::MaxBasal {
            value: BasalRate::from_milliunits_per_hour(cap(a, DEFAULT_BASAL_CAP)),
            previous: BasalRate::from_milliunits_per_hour(cap(b, DEFAULT_BASAL_CAP)),
        }),
        _ => return None,
    })
}

fn clamp(offset: i64) -> i32 {
    offset.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn clock_step(raw: &Raw) -> Option<i64> {
    let [a, b, c] = raw.values.map(i64::from);
    let secs = i64::from(raw.seconds);
    match raw.kind {
        12 if (1..=31).contains(&a) && (1..=12).contains(&b) => {
            Some((days_from_civil(2_000 + c, b, a) - 10_957 - secs.div_euclid(86_400)) * 86_400)
        }
        13 if a <= 23 && b <= 59 && c <= 59 => {
            Some(a * 3_600 + b * 60 + c - secs.rem_euclid(86_400))
        }
        _ => None,
    }
}

#[cfg(feature = "delivery")]
fn time_second(raw: &Raw) -> Option<u64> {
    let [hour, minute, second] = raw.values;
    (raw.kind == 13 && hour <= 23 && minute <= 59 && second <= 59).then_some(u64::from(second))
}

fn running(raw: &Raw) -> bool {
    matches!(
        event(raw),
        Some(HistoryEvent::Bolus {
            in_progress: true,
            ..
        })
    )
}

fn alarm(raw: &Raw) -> Option<HistoryEvent> {
    let kind = match raw.kind {
        101 => AlarmKind::BatteryEmpty,
        104 => AlarmKind::ReservoirEmpty,
        105 => AlarmKind::Occlusion,
        106 => AlarmKind::AutoOff,
        100 | 102 | 103 | 107 | 108 => AlarmKind::Other,
        _ => return None,
    };
    Some(HistoryEvent::Alarm(Alarm {
        kind,
        code: u16::from(raw.kind),
    }))
}

fn complaint(raw: &Raw) -> Option<HistoryEvent> {
    let alarm = Alarm {
        kind: AlarmKind::Other,
        code: raw.values[0],
    };
    match raw.kind {
        204 => Some(HistoryEvent::AlarmCleared(alarm)),
        206 => Some(HistoryEvent::Alarm(alarm)),
        _ => None,
    }
}

impl Ctx {
    fn wide_index(&self) -> bool {
        self.link
            .borrow()
            .as_ref()
            .is_some_and(|v| v.history >= 1.2)
    }

    async fn entry(&self, log: Log, index: u16) -> Step<Option<Result<Raw, ()>>> {
        let [_, select, value] = log.characteristics();
        let speculative = self.read(value).await;
        if let Ok(raw) = speculative.as_deref().map(parse) {
            if let Some(raw) = raw.filter(|r| r.index == index) {
                return Ok(Some(Ok(raw)));
            }
        }
        let bytes = if self.wide_index() {
            safevar(i32::from(index))
        } else {
            index.to_le_bytes().to_vec()
        };
        match self.write(select, WriteClass::Selection, &bytes).await {
            Err(Halt::Failed(Failure::Status(VALUE_INVALID | INDEX_INVALID))) => return Ok(None),
            other => other?,
        }
        match self.read(value).await {
            Err(Halt::Failed(Failure::Status(VALUE_INVALID | INDEX_INVALID))) => Ok(None),
            other => Ok(Some(parse(&other?).ok_or(()))),
        }
    }

    async fn walk(
        &self,
        log: Log,
        stop: impl Fn(&Raw) -> bool,
        limit: Option<u16>,
    ) -> Step<(Vec<Raw>, Option<u32>, Option<Sequence>)> {
        let [count, ..] = log.characteristics();
        let mut collected = (Vec::new(), None, None);
        for _ in 0..2 {
            let count = super::link::le_i32(&self.read_sized(count, 4).await?, 0);
            let mut raws: Vec<Raw> = Vec::new();
            let mut cutoff = None;
            let mut newest = None;
            let mut changed = false;
            for index in 0..u16::try_from(count.max(0)).unwrap_or(u16::MAX) {
                // The bound stops the walk once the newest `limit` entries are in
                // hand, and carries their oldest timestamp into `cutoff` so the
                // alarm and complaint logs are held to the same window.
                if limit.is_some_and(|n| raws.len() >= usize::from(n)) {
                    cutoff = raws.last().map(|raw| raw.seconds);
                    break;
                }
                let raw = match self.entry(log, index).await? {
                    None => break,
                    Some(Err(())) => continue,
                    Some(Ok(raw)) => raw,
                };
                // The first record read is the newest the pump holds (index 0),
                // reported even when `stop` or the bound keeps it out of the page.
                newest.get_or_insert_with(|| log.sequence(&raw));
                if stop(&raw) {
                    cutoff = Some(raw.seconds);
                    break;
                }
                if raws.last().is_some_and(|p| raw.sequence >= p.sequence) {
                    changed = true;
                    break;
                }
                raws.push(raw);
            }
            if !changed {
                return Ok((raws, cutoff, newest));
            }
            collected = (raws, cutoff, newest);
        }
        Ok(collected)
    }

    pub(crate) async fn history(
        &self,
        after: Sequence,
        limit: Option<u16>,
    ) -> Step<(Vec<HistoryEntry>, Sequence)> {
        let after = after.get();
        let cursor = (after >> LOG_SHIFT == Log::Event as u32).then_some(after & SEQUENCE_MASK);
        let stop = |raw: &Raw| cursor.is_some_and(|c| raw.sequence <= c && !running(raw));
        let (raws, cutoff, newest) = self.walk(Log::Event, stop, limit).await?;
        // Log-encoded like the entries' own sequence, so the host compares it
        // against the page and its cursor; the event-tagged zero on an empty log.
        let newest = newest.unwrap_or_else(|| Sequence::new((Log::Event as u32) << LOG_SHIFT));
        let mut entries = self.entries(Log::Event, raws);
        for log in [Log::Alarm, Log::Complaint] {
            let older = |raw: &Raw| cutoff.is_some_and(|s| raw.seconds < s);
            let (raws, ..) = self.walk(log, older, None).await?;
            entries.extend(self.entries(log, raws));
        }
        Ok((entries, newest))
    }

    fn entries(&self, log: Log, raws: Vec<Raw>) -> Vec<HistoryEntry> {
        let mut offset = i64::from(self.utc_offset);
        raws.into_iter()
            .filter_map(|raw| {
                let here = offset;
                if matches!(log, Log::Event) {
                    if let Some(step) = clock_step(&raw) {
                        offset -= step;
                    }
                }
                let event = log.decode(&raw)?;
                let pump_time = raw.pump_time();
                Some(HistoryEntry {
                    sequence: log.sequence(&raw),
                    time: pump_time.to_utc(clamp(here)),
                    pump_time: Some(pump_time),
                    event,
                    raw: raw.bytes,
                })
            })
            .collect()
    }

    #[cfg(feature = "delivery")]
    pub(crate) async fn newest_sequence(&self) -> Step<u32> {
        let [count, ..] = Log::Event.characteristics();
        if super::link::le_i32(&self.read_sized(count, 4).await?, 0) <= 0 {
            return Ok(0);
        }
        Ok(match self.entry(Log::Event, 0).await? {
            Some(Ok(raw)) => raw.sequence,
            _ => 0,
        })
    }

    #[cfg(feature = "delivery")]
    pub(crate) async fn replay(&self, after: u32) -> Step<(InsulinAmount, Option<u64>)> {
        let (raws, ..) = self
            .walk(Log::Event, |raw| raw.sequence <= after, None)
            .await?;
        let mut delivered = 0u32;
        let mut seconds: Vec<u64> = Vec::new();
        for raw in &raws {
            if let Some(HistoryEvent::Bolus {
                kind: BolusKind::Normal,
                delivered: step,
                in_progress: false,
                ..
            }) = event(raw)
            {
                delivered = delivered.saturating_add(step.milliunits());
            }
            if let Some(second) = time_second(raw) {
                seconds.push(second);
            }
        }
        let offset = match seconds.as_slice() {
            [newer, older, ..] => Some((newer + 60 - older) % 60),
            _ => None,
        };
        Ok((InsulinAmount::from_milliunits(delivered), offset))
    }

    #[cfg(feature = "delivery")]
    pub(crate) async fn delivered_since(&self, sequence: u32) -> Step<Option<InsulinAmount>> {
        let (raws, ..) = self
            .walk(Log::Event, |raw| raw.sequence <= sequence, None)
            .await?;
        Ok(raws.iter().find_map(|raw| match event(raw) {
            Some(HistoryEvent::Bolus {
                kind: BolusKind::Normal,
                delivered,
                in_progress: false,
                ..
            }) => Some(delivered),
            _ => None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: u8, values: [u16; 3], sequence: u32) -> Raw {
        Raw {
            seconds: 86_400,
            kind,
            values,
            sequence,
            index: 0,
            bytes: Vec::new(),
        }
    }

    #[test]
    fn one_pump_sequence_is_three_sequences_one_per_log() {
        let entry = raw(16, [0; 3], 7);
        let sequences = [Log::Alarm, Log::Complaint, Log::Event].map(|log| log.sequence(&entry));
        assert_eq!(
            sequences.map(Sequence::get),
            [(1 << 30) | 7, (2 << 30) | 7, (3 << 30) | 7]
        );
    }

    #[test]
    fn each_log_reads_its_own_types() {
        let entry = raw(206, [17, 0, 0], 1);
        assert_eq!(Log::Event.decode(&entry), None);
        assert_eq!(Log::Alarm.decode(&entry), None);
        assert_eq!(
            Log::Complaint.decode(&entry),
            Some(HistoryEvent::Alarm(Alarm {
                kind: AlarmKind::Other,
                code: 17
            }))
        );
        assert_eq!(
            Log::Event.decode(&raw(13, [1, 2, 3], 1)),
            Some(HistoryEvent::ClockChanged {
                previous: PumpTime::from_local_seconds(pump_seconds(86_400))
            })
        );
    }

    #[test]
    fn clock_step_reads_time_and_date_changes_and_rejects_the_impossible() {
        assert_eq!(clock_step(&raw(13, [0, 0, 0], 1)), Some(0));
        assert_eq!(clock_step(&raw(13, [1, 0, 0], 1)), Some(3_600));
        assert_eq!(clock_step(&raw(13, [24, 0, 0], 1)), None);
        assert_eq!(clock_step(&raw(13, [0, 60, 0], 1)), None);
        assert_eq!(clock_step(&raw(13, [0, 0, 60], 1)), None);
        assert_eq!(clock_step(&raw(12, [3, 1, 0], 1)), Some(86_400));
        assert_eq!(clock_step(&raw(12, [0, 1, 0], 1)), None);
        assert_eq!(clock_step(&raw(12, [1, 13, 0], 1)), None);
        assert_eq!(clock_step(&raw(16, [0, 0, 0], 1)), None);
    }

    #[test]
    fn days_from_civil_matches_known_dates_across_the_leap_rules() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(-1, 2, 28), -719_835);
    }

    #[test]
    fn an_offset_beyond_i32_is_clamped() {
        assert_eq!(clamp(i64::MAX), i32::MAX);
        assert_eq!(clamp(i64::MIN), i32::MIN);
        assert_eq!(clamp(5), 5);
    }
}
