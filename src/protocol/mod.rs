mod characteristic;
mod crypto;
mod error;
mod framing;
pub mod setting;
mod units;
pub mod uuids;

pub use characteristic::Characteristic;
pub use crypto::{
    Crypto, Opened, COUNTER_TRAILER_LEN, KEY_LEN, MAX_PAYLOAD_LEN, MIN_SEALED_LEN, NONCE_LEN,
    TAG_LEN,
};
pub use error::{
    AttError, ProtoError, PumpError, Status, ATT_INSUFFICIENT_AUTHENTICATION,
    ATT_INSUFFICIENT_AUTHORIZATION, ATT_INSUFFICIENT_ENCRYPTION, ERR_COUNTER_ERROR,
    ERR_ENCRYPTION_FAILED, ERR_INVALID_ID, ERR_NO_SHARED_KEY, ERR_VALUE_CORRUPT, PUMP_ERROR_BASE,
};
pub use framing::{Frames, Reassembler, MAX_FRAMES, MAX_FRAME_PAYLOAD, MAX_MESSAGE_LEN};
pub use units::{assemble_serial, hundredths_to_units, units_to_hundredths};
pub use uuids::Service;
