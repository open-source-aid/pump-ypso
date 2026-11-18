use alloc::vec;
use alloc::vec::Vec;

use super::ProtoError;

pub const MAX_FRAME_PAYLOAD: usize = 19;
pub const MAX_FRAMES: usize = 0x0F;
pub const MAX_MESSAGE_LEN: usize = MAX_FRAMES * MAX_FRAME_PAYLOAD;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frames;

impl Frames {
    pub fn header(frame: u8, total: u8) -> u8 {
        ((frame << 4) & 0xF0) | (total & 0x0F)
    }

    pub fn parse_header(byte: u8) -> (u8, u8) {
        let frame = byte >> 4;
        let total = match byte & 0x0F {
            0 => 1,
            n => n,
        };
        (frame, total)
    }

    pub fn split(payload: &[u8]) -> Result<Vec<Vec<u8>>, ProtoError> {
        if payload.is_empty() {
            return Ok(vec![vec![Self::header(1, 1)]]);
        }
        if payload.len() > MAX_MESSAGE_LEN {
            return Err(ProtoError::PayloadTooLong {
                len: payload.len(),
                max: MAX_FRAMES,
            });
        }
        let total = payload.len().div_ceil(MAX_FRAME_PAYLOAD) as u8;
        let frames = payload
            .chunks(MAX_FRAME_PAYLOAD)
            .zip(1..)
            .map(|(chunk, frame)| {
                let mut bytes = Vec::with_capacity(1 + chunk.len());
                bytes.push(Self::header(frame, total));
                bytes.extend_from_slice(chunk);
                bytes
            })
            .collect();
        Ok(frames)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reassembler {
    total: Option<u8>,
    received: u8,
    buf: Vec<u8>,
}

impl Reassembler {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn needs_continuation(&self) -> bool {
        self.total.is_some_and(|total| self.received < total)
    }

    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, ProtoError> {
        let Some((&header, payload)) = frame.split_first() else {
            return Err(ProtoError::EmptyFrame);
        };
        let (this_frame, total) = Frames::parse_header(header);
        if this_frame == 0 || this_frame > total {
            return Err(ProtoError::InvalidHeader {
                byte: header,
                frame: this_frame,
                total,
            });
        }
        let expected = self.received + 1;
        match self.total {
            None if this_frame != 1 => {
                return Err(ProtoError::OutOfOrderFrame {
                    expected: 1,
                    got: this_frame,
                })
            }
            None => self.total = Some(total),
            Some(_) if this_frame == 1 => {
                return Err(ProtoError::StaleFrames {
                    buffered: self.buf.len(),
                })
            }
            Some(expected_total) if total != expected_total => {
                return Err(ProtoError::FrameCountMismatch {
                    expected: expected_total,
                    got: total,
                })
            }
            Some(_) if this_frame != expected => {
                return Err(ProtoError::OutOfOrderFrame {
                    expected,
                    got: this_frame,
                })
            }
            Some(_) => {}
        }
        self.buf.extend_from_slice(payload);
        self.received = expected;
        if self.received >= total {
            Ok(Some(core::mem::take(self).buf))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(payload: &[u8]) -> Vec<Vec<u8>> {
        Frames::split(payload).unwrap()
    }

    #[test]
    fn header_encode_matches_handover_examples() {
        assert_eq!(Frames::header(1, 4), 0x14);
        assert_eq!(Frames::header(4, 4), 0x44);
        assert_eq!(Frames::parse_header(0x14), (1, 4));
        assert_eq!(Frames::parse_header(0x44), (4, 4));
    }

    #[test]
    fn header_round_trips_for_every_valid_pair() {
        for total in 1..=15u8 {
            for frame in 1..=total {
                let byte = Frames::header(frame, total);
                assert_eq!(
                    Frames::parse_header(byte),
                    (frame, total),
                    "byte {byte:#04X}"
                );
            }
        }
    }

    #[test]
    fn zero_total_nibble_reads_as_one_frame() {
        assert_eq!(Frames::parse_header(0x10), (1, 1));
    }

    #[test]
    fn frames_are_one_indexed_not_zero_indexed() {
        assert_eq!(split(&[0u8; 5])[0][0] >> 4, 1);
    }

    fn round_trip(payload: &[u8]) -> Vec<u8> {
        let frames = split(payload);
        let mut reassembler = Reassembler::default();
        let mut out = None;
        for (index, frame) in frames.iter().enumerate() {
            let result = reassembler.push(frame).unwrap();
            if index + 1 < frames.len() {
                assert_eq!(result, None);
                assert!(reassembler.needs_continuation());
            } else {
                out = result;
            }
        }
        assert!(!reassembler.needs_continuation());
        out.unwrap()
    }

    #[test]
    fn round_trips_one_frame() {
        let payload: Vec<u8> = (0..19).collect();
        assert_eq!(split(&payload).len(), 1);
        assert_eq!(round_trip(&payload), payload);
    }

    #[test]
    fn round_trips_two_frames() {
        let payload: Vec<u8> = (0..20).collect();
        let frames = split(&payload);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0][0], 0x12);
        assert_eq!(frames[1][0], 0x22);
        assert_eq!(frames[1].len(), 2);
        assert_eq!(round_trip(&payload), payload);
    }

    #[test]
    fn round_trips_four_frames() {
        let payload: Vec<u8> = (0..76u8).map(|i| i.wrapping_mul(7)).collect();
        let frames = split(&payload);
        assert_eq!(
            frames.iter().map(|f| f[0]).collect::<Vec<_>>(),
            vec![0x14, 0x24, 0x34, 0x44]
        );
        assert_eq!(round_trip(&payload), payload);
    }

    #[test]
    fn round_trips_every_length_up_to_the_frame_ceiling() {
        for len in 1..=MAX_MESSAGE_LEN {
            let payload: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
            let frames = split(&payload);
            assert_eq!(frames.len(), len.div_ceil(MAX_FRAME_PAYLOAD), "len {len}");
            assert!(frames.iter().all(|f| f.len() <= 1 + MAX_FRAME_PAYLOAD));
            assert_eq!(round_trip(&payload), payload, "len {len}");
        }
    }

    #[test]
    fn fifteen_frames_carry_total_fifteen_in_every_header() {
        let frames = split(&[0u8; MAX_MESSAGE_LEN]);
        let headers: Vec<u8> = frames.iter().map(|f| f[0]).collect();
        let expected: Vec<u8> = (1..=15u8).map(|frame| (frame << 4) | 0x0F).collect();
        assert_eq!(headers, expected);
    }

    #[test]
    fn empty_payload_still_produces_one_frame() {
        assert_eq!(split(&[]), vec![vec![0x11]]);
    }

    #[test]
    fn oversize_payload_is_rejected_not_wrapped() {
        assert_eq!(
            Frames::split(&[0u8; MAX_MESSAGE_LEN + 1]),
            Err(ProtoError::PayloadTooLong {
                len: MAX_MESSAGE_LEN + 1,
                max: MAX_FRAMES
            })
        );
    }

    #[test]
    fn reassembler_rejects_out_of_order_frames() {
        let frames = split(&[0u8; 76]);
        let mut reassembler = Reassembler::default();
        reassembler.push(&frames[0]).unwrap();
        assert_eq!(
            reassembler.push(&frames[2]),
            Err(ProtoError::OutOfOrderFrame {
                expected: 2,
                got: 3
            })
        );
    }

    #[test]
    fn reassembler_rejects_duplicate_frames() {
        let frames = split(&[0u8; 76]);
        let mut reassembler = Reassembler::default();
        reassembler.push(&frames[0]).unwrap();
        reassembler.push(&frames[1]).unwrap();
        assert_eq!(
            reassembler.push(&frames[1]),
            Err(ProtoError::OutOfOrderFrame {
                expected: 3,
                got: 2
            })
        );
    }

    #[test]
    fn reassembler_rejects_a_repeated_first_frame_as_stale() {
        let frames = split(&[0u8; 76]);
        let mut reassembler = Reassembler::default();
        reassembler.push(&frames[0]).unwrap();
        reassembler.push(&frames[1]).unwrap();
        assert_eq!(
            reassembler.push(&frames[0]),
            Err(ProtoError::StaleFrames { buffered: 38 })
        );
        reassembler.reset();
        assert_eq!(reassembler, Reassembler::default());
        assert!(reassembler.push(&frames[0]).unwrap().is_none());
    }

    #[test]
    fn reassembler_rejects_a_frame_from_a_different_message() {
        let mut reassembler = Reassembler::default();
        reassembler.push(&[Frames::header(1, 4), 1, 2, 3]).unwrap();
        assert_eq!(
            reassembler.push(&[Frames::header(2, 6), 4, 5, 6]),
            Err(ProtoError::FrameCountMismatch {
                expected: 4,
                got: 6
            })
        );
    }

    #[test]
    fn reassembler_rejects_a_continuation_frame_when_idle() {
        let mut reassembler = Reassembler::default();
        assert_eq!(
            reassembler.push(&[Frames::header(2, 4), 9]),
            Err(ProtoError::OutOfOrderFrame {
                expected: 1,
                got: 2
            })
        );
    }

    #[test]
    fn reassembler_rejects_frame_zero_and_frame_past_total() {
        let mut reassembler = Reassembler::default();
        assert_eq!(
            reassembler.push(&[0x04, 1, 2]),
            Err(ProtoError::InvalidHeader {
                byte: 0x04,
                frame: 0,
                total: 4
            })
        );
        assert_eq!(
            reassembler.push(&[0x52, 1, 2]),
            Err(ProtoError::InvalidHeader {
                byte: 0x52,
                frame: 5,
                total: 2
            })
        );
    }

    #[test]
    fn reassembler_rejects_an_empty_frame() {
        assert_eq!(
            Reassembler::default().push(&[]),
            Err(ProtoError::EmptyFrame)
        );
    }

    #[test]
    fn needs_continuation_is_false_before_and_after_a_message() {
        let frames = split(&[7u8; 40]);
        let mut reassembler = Reassembler::default();
        assert!(!reassembler.needs_continuation());
        reassembler.push(&frames[0]).unwrap();
        assert!(reassembler.needs_continuation());
        reassembler.push(&frames[1]).unwrap();
        assert!(reassembler.needs_continuation());
        assert_eq!(reassembler.push(&frames[2]).unwrap(), Some(vec![7u8; 40]));
        assert!(!reassembler.needs_continuation());
        assert_eq!(reassembler, Reassembler::default());
    }

    #[test]
    fn a_bare_single_frame_header_completes_an_empty_message() {
        assert_eq!(Reassembler::default().push(&[0x10]), Ok(Some(Vec::new())));
    }

    #[test]
    fn a_rejected_frame_does_not_corrupt_the_buffer() {
        let frames = split(&[3u8; 57]);
        let mut reassembler = Reassembler::default();
        reassembler.push(&frames[0]).unwrap();
        assert!(reassembler.push(&frames[2]).is_err());
        reassembler.push(&frames[1]).unwrap();
        assert_eq!(reassembler.push(&frames[2]).unwrap(), Some(vec![3u8; 57]));
    }
}
