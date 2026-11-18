use alloc::vec::Vec;

use pump_shared::{BleResult, Uuid};

use super::YpsoSim;
use crate::protocol::{Crypto, Frames, ERR_COUNTER_ERROR, ERR_ENCRYPTION_FAILED, NONCE_LEN};

const ERR_NO_KEY: u8 = 0x8C;
const ERR_FRAME_REJECTED: u8 = 0x8D;
const ATT_INVALID_OFFSET: u8 = 0x07;
const INBOX_CAPACITY: usize = 286;

#[derive(Debug)]
pub(super) struct Inbox {
    context: Uuid,
    total: u8,
    received: u8,
    buf: Vec<u8>,
}

impl YpsoSim {
    pub(super) fn reply(&mut self, plaintext: Vec<u8>) -> BleResult {
        if !self.crypto_service {
            return BleResult::Data(plaintext);
        }
        let Some(key) = self.key else {
            return BleResult::Status(ERR_NO_KEY);
        };
        self.pump_counter += 1;
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..8].copy_from_slice(&self.pump_counter.to_le_bytes());
        nonce[8..12].copy_from_slice(&self.reboot.to_le_bytes());
        let sealed = Crypto::new(key)
            .seal(&plaintext, self.reboot, self.pump_counter, &nonce)
            .expect("every simulated reply fits one message");
        self.framed(&sealed)
    }

    pub(super) fn framed(&mut self, message: &[u8]) -> BleResult {
        self.tx = Frames::split(message)
            .expect("every simulated reply fits the frames")
            .into();
        BleResult::Data(self.tx.pop_front().expect("a split yields a frame"))
    }

    pub(super) fn continuation(&mut self) -> BleResult {
        self.tx
            .pop_front()
            .map_or(BleResult::Status(ATT_INVALID_OFFSET), BleResult::Data)
    }

    pub(super) fn receive(
        &mut self,
        context: Uuid,
        frame: &[u8],
    ) -> Result<Option<Vec<u8>>, BleResult> {
        let pending = self.rx.take();
        let header = frame.first().copied().unwrap_or_default();
        let (sequence, total) = (header >> 4, header & 0x0F);
        let mut buf = match pending {
            None if sequence == 1 => Vec::new(),
            Some(inbox)
                if inbox.context == context
                    && inbox.total == total
                    && sequence == inbox.received + 1 =>
            {
                inbox.buf
            }
            _ => return Err(BleResult::Status(ERR_FRAME_REJECTED)),
        };
        if frame.len() < 2 || sequence > total || buf.len() + frame.len() - 1 > INBOX_CAPACITY {
            return Err(BleResult::Status(ERR_FRAME_REJECTED));
        }
        buf.extend_from_slice(&frame[1..]);
        if sequence == total {
            return Ok(Some(buf));
        }
        self.rx = Some(Inbox {
            context,
            total,
            received: sequence,
            buf,
        });
        Ok(None)
    }

    pub(super) fn message(
        &mut self,
        context: Uuid,
        data: &[u8],
    ) -> Result<Option<Vec<u8>>, BleResult> {
        if self.crypto_service {
            self.receive(context, data)
        } else {
            Ok(Some(data.to_vec()))
        }
    }

    pub(super) fn open(&mut self, message: Vec<u8>) -> Result<Vec<u8>, u8> {
        if !self.crypto_service {
            return Ok(message);
        }
        let opened = self.key.ok_or(ERR_ENCRYPTION_FAILED).and_then(|key| {
            Crypto::new(key)
                .open(&message)
                .map_err(|_| ERR_ENCRYPTION_FAILED)
        })?;
        if opened.reboot != self.reboot || opened.counter as i64 <= self.app_counter as i64 {
            return Err(ERR_COUNTER_ERROR);
        }
        self.app_counter = opened.counter;
        Ok(opened.payload)
    }
}
