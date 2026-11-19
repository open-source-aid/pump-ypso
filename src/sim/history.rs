use alloc::vec::Vec;

use pump_shared::BleResult;

use super::codec::{parse_safevar, safevar};
use super::settings::ERR_VALUE_INVALID;
use crate::engine::crc::with_crc16;
use crate::protocol::ERR_VALUE_CORRUPT;

const ERR_ENTRY_INDEX_INVALID: u8 = 0x84;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub timestamp: u32,
    pub kind: u8,
    pub values: [u16; 3],
    pub sequence: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    pub entries: Vec<Entry>,
    pub capacity: Option<usize>,
    index: usize,
}

impl Log {
    pub fn push(&mut self, timestamp: u32, kind: u8, values: [u16; 3]) -> u32 {
        let sequence = self.entries.last().map_or(0, |entry| entry.sequence) + 1;
        self.entries.push(Entry {
            timestamp,
            kind,
            values,
            sequence,
        });
        sequence
    }

    pub(super) fn rewrite(&mut self, sequence: u32, kind: u8, values: [u16; 3]) {
        for entry in self.entries.iter_mut().filter(|e| e.sequence == sequence) {
            entry.kind = kind;
            entry.values = values;
        }
    }

    fn reported(&self) -> usize {
        self.capacity.unwrap_or(self.entries.len())
    }

    pub(super) fn count(&self) -> Vec<u8> {
        safevar(self.reported() as i32).to_vec()
    }

    pub(super) fn select(&mut self, data: &[u8], safevar_width: bool) -> BleResult {
        let index = if safevar_width {
            parse_safevar(data)
        } else {
            <[u8; 2]>::try_from(data)
                .ok()
                .map(|bytes| i32::from(u16::from_le_bytes(bytes)))
        };
        match index.map(usize::try_from) {
            None => BleResult::Status(ERR_VALUE_CORRUPT),
            Some(Ok(index)) if index < self.reported() => {
                self.index = index;
                BleResult::Done
            }
            Some(_) => BleResult::Status(ERR_VALUE_INVALID),
        }
    }

    pub(super) fn value(&mut self) -> Result<Vec<u8>, BleResult> {
        let index = self.index;
        let Some(entry) = self.entries.iter().rev().nth(index) else {
            return Err(BleResult::Status(ERR_ENTRY_INDEX_INVALID));
        };
        let mut bytes = entry.timestamp.to_le_bytes().to_vec();
        bytes.push(entry.kind);
        for value in entry.values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&entry.sequence.to_le_bytes());
        bytes.extend_from_slice(&(index as u16).to_le_bytes());
        self.index += 1;
        Ok(with_crc16(bytes))
    }
}
