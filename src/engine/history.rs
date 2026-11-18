use alloc::vec::Vec;
use core::time::Duration;

use pump_shared::{
    BolusKind, Component, Failure, HistoryEntry, HistoryEvent, InsulinAmount, Percent, PumpTime,
    Sequence, TempBasalRate,
};

use super::crc::crc16_ok;
use super::link::{safevar, Ctx, Halt, Step, WriteClass};
use super::pump::pump_seconds;
use crate::Characteristic;

const ENTRY_LEN: usize = 19;
const VALUE_INVALID: u8 = 0x82;
const INDEX_INVALID: u8 = 0x84;

struct Raw {
    seconds: u32,
    kind: u8,
    values: [u16; 3],
    sequence: u32,
    index: u16,
    bytes: Vec<u8>,
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

fn decode(raw: &Raw) -> Option<HistoryEvent> {
    let [a, b, c] = raw.values;
    let amount = |v: u16| InsulinAmount::from_milliunits(u32::from(v) * 10);
    let minutes = |v: u16| Duration::from_secs(u64::from(v) * 60);
    let bolus = |kind, requested: u16, delivered: u16, duration, in_progress| HistoryEvent::Bolus {
        kind,
        requested: amount(requested),
        delivered: amount(delivered),
        duration,
        automatic: false,
        in_progress,
    };
    Some(match raw.kind {
        2 | 26 | 29 | 28 => bolus(BolusKind::Normal, a, a, Duration::ZERO, false),
        19 | 27 => bolus(BolusKind::Normal, a, 0, Duration::ZERO, true),
        3 => bolus(BolusKind::Extended, a, a, minutes(b), false),
        1 => bolus(BolusKind::Extended, a, 0, minutes(b), true),
        18 => bolus(BolusKind::Extended, a, a, minutes(c), false),
        17 => bolus(BolusKind::Extended, a, 0, minutes(c), true),
        30 | 31 => HistoryEvent::ExtendedBolusEnded {
            delivered: amount(a),
        },
        9 | 10 => HistoryEvent::TempBasal {
            rate: TempBasalRate::Percent(Percent::new(a)),
            duration: minutes(b),
        },
        32 => HistoryEvent::TempBasalEnded,
        6..=8 => HistoryEvent::BasalProfileChanged,
        14 if a == 10 => HistoryEvent::Resumed,
        14 => HistoryEvent::Suspended,
        16 => HistoryEvent::Rewound,
        24 => HistoryEvent::Replaced(Component::Battery),
        25 => HistoryEvent::Replaced(Component::Cannula),
        _ => return None,
    })
}

impl Ctx {
    async fn entry(&self, index: u16, wide: bool) -> Step<Option<Result<Raw, ()>>> {
        let speculative = self.read(Characteristic::EventEntryValue).await;
        if let Ok(raw) = speculative.as_deref().map(parse) {
            if let Some(raw) = raw.filter(|r| r.index == index) {
                return Ok(Some(Ok(raw)));
            }
        }
        let value = if wide {
            safevar(i32::from(index))
        } else {
            index.to_le_bytes().to_vec()
        };
        match self
            .write(
                Characteristic::EventEntryIndex,
                WriteClass::Selection,
                &value,
            )
            .await
        {
            Err(Halt::Failed(Failure::Status(VALUE_INVALID | INDEX_INVALID))) => return Ok(None),
            other => other?,
        }
        match parse(&self.read(Characteristic::EventEntryValue).await?) {
            Some(raw) if raw.index != index => Err(Halt::Failed(Failure::Protocol)),
            Some(raw) => Ok(Some(Ok(raw))),
            None => Ok(Some(Err(()))),
        }
    }

    pub(crate) async fn history(&self, after: Sequence) -> Step<Vec<HistoryEntry>> {
        let count = super::link::le_i32(
            &self.read_sized(Characteristic::EventEntryCount, 4).await?,
            0,
        );
        let wide = self
            .link
            .borrow()
            .as_ref()
            .is_some_and(|v| v.history >= 1.2);
        for _ in 0..2 {
            let mut entries: Vec<HistoryEntry> = Vec::new();
            let mut previous = None;
            let mut changed = false;
            for index in 0..u16::try_from(count.max(0)).unwrap_or(u16::MAX) {
                let raw = match self.entry(index, wide).await? {
                    None => break,
                    Some(Err(())) => continue,
                    Some(Ok(raw)) => raw,
                };
                if raw.sequence <= after.get() {
                    break;
                }
                match previous {
                    Some(p) if raw.sequence == p => continue,
                    Some(p) if raw.sequence > p => {
                        changed = true;
                        break;
                    }
                    _ => previous = Some(raw.sequence),
                }
                if let Some(event) = decode(&raw) {
                    let local = pump_seconds(raw.seconds);
                    let pump_time = PumpTime::from_local_seconds(local);
                    entries.push(HistoryEntry {
                        sequence: Sequence::new(raw.sequence),
                        time: pump_time.to_utc(self.utc_offset),
                        pump_time: Some(pump_time),
                        event,
                        raw: raw.bytes,
                    });
                }
            }
            if !changed {
                return Ok(entries);
            }
        }
        Err(Halt::Failed(Failure::Protocol))
    }

    #[cfg(feature = "delivery")]
    pub(crate) async fn newest_sequence(&self) -> Step<u32> {
        let wide = self
            .link
            .borrow()
            .as_ref()
            .is_some_and(|v| v.history >= 1.2);
        Ok(match self.entry(0, wide).await? {
            Some(Ok(raw)) => raw.sequence,
            _ => 0,
        })
    }

    #[cfg(feature = "delivery")]
    pub(crate) async fn delivered_since(&self, sequence: u32) -> Step<Option<InsulinAmount>> {
        for entry in self.history(Sequence::new(sequence)).await? {
            if let HistoryEvent::Bolus {
                kind: BolusKind::Normal,
                delivered,
                in_progress: false,
                ..
            } = entry.event
            {
                return Ok(Some(delivered));
            }
        }
        Ok(None)
    }
}
