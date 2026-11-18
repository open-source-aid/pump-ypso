use core::fmt;

pub const PUMP_ERROR_BASE: u8 = 0x80;

pub const ATT_INSUFFICIENT_AUTHENTICATION: u8 = 0x05;
pub const ATT_INSUFFICIENT_AUTHORIZATION: u8 = 0x08;
pub const ATT_INSUFFICIENT_ENCRYPTION: u8 = 0x0F;

pub const ERR_VALUE_CORRUPT: u8 = 0x81;
pub const ERR_INVALID_ID: u8 = 0x83;
pub const ERR_ENCRYPTION_FAILED: u8 = 0x8A;
pub const ERR_COUNTER_ERROR: u8 = 0x8B;
pub const ERR_NO_SHARED_KEY: u8 = 0x8C;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttError {
    InsufficientAuthentication,
    InsufficientAuthorization,
    InsufficientEncryption,
    Other(u8),
}

impl AttError {
    pub fn code(self) -> u8 {
        match self {
            Self::InsufficientAuthentication => ATT_INSUFFICIENT_AUTHENTICATION,
            Self::InsufficientAuthorization => ATT_INSUFFICIENT_AUTHORIZATION,
            Self::InsufficientEncryption => ATT_INSUFFICIENT_ENCRYPTION,
            Self::Other(code) => code,
        }
    }
}

impl fmt::Display for AttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsufficientAuthentication => f.write_str(
                "ATT INSUFFICIENT_AUTHENTICATION (0x05): bonding in progress, wait and retry",
            ),
            Self::InsufficientAuthorization => f.write_str(
                "ATT INSUFFICIENT_AUTHORIZATION (0x08): authorisation credential refused",
            ),
            Self::InsufficientEncryption => f.write_str(
                "ATT INSUFFICIENT_ENCRYPTION (0x0F): the link must be encrypted, bond first",
            ),
            Self::Other(code) => write!(f, "ATT error 0x{code:02X}"),
        }
    }
}

impl core::error::Error for AttError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PumpError {
    EncryptionFailed,
    CounterError,
    NoSharedKey,
    Other(u8),
}

impl PumpError {
    pub fn from_status(status: u8) -> Option<Self> {
        Status::classify(status).pump_error()
    }

    pub fn code(self) -> u8 {
        match self {
            Self::EncryptionFailed => ERR_ENCRYPTION_FAILED,
            Self::CounterError => ERR_COUNTER_ERROR,
            Self::NoSharedKey => ERR_NO_SHARED_KEY,
            Self::Other(code) => code,
        }
    }

    pub fn is_retryable(self) -> bool {
        matches!(self, Self::CounterError)
    }
}

impl fmt::Display for PumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EncryptionFailed => {
                f.write_str("pump reports ENCRYPTION_FAILED (0x8A): key is wrong, re-pair")
            }
            Self::CounterError => {
                f.write_str("pump reports COUNTER_ERROR (0x8B): write counter behind, jump forward")
            }
            Self::NoSharedKey => f.write_str("pump reports NO_SHARED_KEY (0x8C): re-pair"),
            Self::Other(code) => write!(f, "pump reports status 0x{code:02X}"),
        }
    }
}

impl core::error::Error for PumpError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Success,
    Att(AttError),
    Pump(PumpError),
}

impl Status {
    pub fn classify(status: u8) -> Self {
        match status {
            0x00 => Self::Success,
            ATT_INSUFFICIENT_AUTHENTICATION => Self::Att(AttError::InsufficientAuthentication),
            ATT_INSUFFICIENT_AUTHORIZATION => Self::Att(AttError::InsufficientAuthorization),
            ATT_INSUFFICIENT_ENCRYPTION => Self::Att(AttError::InsufficientEncryption),
            other if other < PUMP_ERROR_BASE => Self::Att(AttError::Other(other)),
            ERR_ENCRYPTION_FAILED => Self::Pump(PumpError::EncryptionFailed),
            ERR_COUNTER_ERROR => Self::Pump(PumpError::CounterError),
            ERR_NO_SHARED_KEY => Self::Pump(PumpError::NoSharedKey),
            other => Self::Pump(PumpError::Other(other)),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Success => 0x00,
            Self::Att(error) => error.code(),
            Self::Pump(error) => error.code(),
        }
    }

    pub fn pump_error(self) -> Option<PumpError> {
        match self {
            Self::Pump(error) => Some(error),
            Self::Success | Self::Att(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProtoError {
    EmptyFrame,
    InvalidHeader { byte: u8, frame: u8, total: u8 },
    OutOfOrderFrame { expected: u8, got: u8 },
    FrameCountMismatch { expected: u8, got: u8 },
    StaleFrames { buffered: usize },
    SealedTooShort { len: usize, min: usize },
    Aead,
    PayloadTooLong { len: usize, max: usize },
    PlaintextTooLong { len: usize, max: usize },
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => f.write_str("empty BLE frame"),
            Self::InvalidHeader { byte, frame, total } => write!(
                f,
                "invalid frame header 0x{byte:02X} (frame {frame} of {total})"
            ),
            Self::OutOfOrderFrame { expected, got } => {
                write!(f, "out-of-order frame: expected {expected}, got {got}")
            }
            Self::FrameCountMismatch { expected, got } => write!(
                f,
                "frame count mismatch: message claimed {expected} frames, this frame claims {got}"
            ),
            Self::StaleFrames { buffered } => write!(
                f,
                "frame 1 of a new message arrived with {buffered} bytes still buffered"
            ),
            Self::SealedTooShort { len, min } => write!(
                f,
                "sealed message too short: {len} bytes, need at least {min}"
            ),
            Self::Aead => f.write_str("AEAD authentication failed (wrong key or tampered data)"),
            Self::PayloadTooLong { len, max } => {
                write!(f, "payload of {len} bytes needs more than {max} frames")
            }
            Self::PlaintextTooLong { len, max } => {
                write!(f, "plaintext of {len} bytes exceeds the {max}-byte limit")
            }
        }
    }
}

impl core::error::Error for ProtoError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn insufficient_encryption_is_a_bonding_requirement_not_a_gatt_failure() {
        assert_eq!(
            Status::classify(0x0F),
            Status::Att(AttError::InsufficientEncryption)
        );
        assert_eq!(AttError::InsufficientEncryption.code(), 0x0F);
        assert_eq!(PumpError::from_status(0x0F), None);
    }

    #[test]
    fn the_att_range_is_never_a_pump_application_error() {
        assert_eq!(
            Status::classify(0x08),
            Status::Att(AttError::InsufficientAuthorization)
        );
        assert_eq!(PumpError::from_status(0x08), None);
    }

    #[test]
    fn bonding_in_progress_is_its_own_classification() {
        assert_eq!(
            Status::classify(0x05),
            Status::Att(AttError::InsufficientAuthentication)
        );
        assert_eq!(PumpError::from_status(0x05), None);
    }

    fn is_att(status: Status) -> bool {
        matches!(status, Status::Att(_))
    }

    #[test]
    fn the_whole_byte_range_falls_on_the_right_side_of_the_boundary() {
        assert_eq!(Status::classify(0x00), Status::Success);
        assert_eq!(Status::Success.code(), 0x00);
        assert_eq!(PumpError::from_status(0x00), None);
        for byte in 0x01..PUMP_ERROR_BASE {
            let status = Status::classify(byte);
            assert!(is_att(status), "0x{byte:02X}");
            assert_eq!(status.pump_error(), None, "0x{byte:02X}");
            assert_eq!(status.code(), byte);
        }
        for byte in PUMP_ERROR_BASE..=0xFF {
            let status = Status::classify(byte);
            assert!(!is_att(status), "0x{byte:02X}");
            assert!(status.pump_error().is_some(), "0x{byte:02X}");
            assert_eq!(status.code(), byte);
        }
    }

    #[test]
    fn the_pump_codes_that_drive_recovery_classify_exactly() {
        assert_eq!(
            Status::classify(0x8A),
            Status::Pump(PumpError::EncryptionFailed)
        );
        assert_eq!(
            Status::classify(0x8B),
            Status::Pump(PumpError::CounterError)
        );
        assert_eq!(Status::classify(0x8C), Status::Pump(PumpError::NoSharedKey));
        assert_eq!(PumpError::from_status(0x8B), Some(PumpError::CounterError));
        assert!(PumpError::CounterError.is_retryable());
        assert!(!PumpError::EncryptionFailed.is_retryable());
        assert!(!PumpError::NoSharedKey.is_retryable());
        assert!(!PumpError::Other(0x87).is_retryable());
    }

    #[test]
    fn an_unrecognised_application_byte_is_still_a_pump_error() {
        assert_eq!(Status::classify(0x87), Status::Pump(PumpError::Other(0x87)));
        assert_eq!(PumpError::from_status(0x87), Some(PumpError::Other(0x87)));
        assert_eq!(PumpError::Other(0x87).code(), 0x87);
        assert_eq!(Status::classify(0x13), Status::Att(AttError::Other(0x13)));
    }

    #[test]
    fn att_errors_display_their_meaning() {
        for (error, text) in [
            (
                AttError::InsufficientAuthentication,
                "ATT INSUFFICIENT_AUTHENTICATION (0x05): bonding in progress, wait and retry",
            ),
            (
                AttError::InsufficientAuthorization,
                "ATT INSUFFICIENT_AUTHORIZATION (0x08): authorisation credential refused",
            ),
            (
                AttError::InsufficientEncryption,
                "ATT INSUFFICIENT_ENCRYPTION (0x0F): the link must be encrypted, bond first",
            ),
            (AttError::Other(0x13), "ATT error 0x13"),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }

    #[test]
    fn pump_errors_display_their_meaning() {
        for (error, text) in [
            (
                PumpError::EncryptionFailed,
                "pump reports ENCRYPTION_FAILED (0x8A): key is wrong, re-pair",
            ),
            (
                PumpError::CounterError,
                "pump reports COUNTER_ERROR (0x8B): write counter behind, jump forward",
            ),
            (
                PumpError::NoSharedKey,
                "pump reports NO_SHARED_KEY (0x8C): re-pair",
            ),
            (PumpError::Other(0x87), "pump reports status 0x87"),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }

    #[test]
    fn proto_errors_display_their_details() {
        for (error, text) in [
            (ProtoError::EmptyFrame, "empty BLE frame"),
            (
                ProtoError::InvalidHeader {
                    byte: 0x52,
                    frame: 5,
                    total: 2,
                },
                "invalid frame header 0x52 (frame 5 of 2)",
            ),
            (
                ProtoError::OutOfOrderFrame {
                    expected: 2,
                    got: 3,
                },
                "out-of-order frame: expected 2, got 3",
            ),
            (
                ProtoError::FrameCountMismatch {
                    expected: 4,
                    got: 6,
                },
                "frame count mismatch: message claimed 4 frames, this frame claims 6",
            ),
            (
                ProtoError::StaleFrames { buffered: 38 },
                "frame 1 of a new message arrived with 38 bytes still buffered",
            ),
            (
                ProtoError::SealedTooShort { len: 3, min: 52 },
                "sealed message too short: 3 bytes, need at least 52",
            ),
            (
                ProtoError::Aead,
                "AEAD authentication failed (wrong key or tampered data)",
            ),
            (
                ProtoError::PayloadTooLong { len: 300, max: 15 },
                "payload of 300 bytes needs more than 15 frames",
            ),
            (
                ProtoError::PlaintextTooLong { len: 234, max: 233 },
                "plaintext of 234 bytes exceeds the 233-byte limit",
            ),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }
}
