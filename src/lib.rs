#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod counters;
mod driver;
mod engine;
pub mod protocol;
#[cfg(feature = "sim")]
pub mod sim;

pub use counters::{CounterError, CounterState, Counters, EpochChange};
pub use driver::{Ypso, DRIVER};
pub use engine::{inspect_session, SessionError, SessionSummary};
pub use protocol::{
    AttError, Characteristic, Crypto, Frames, Opened, ProtoError, PumpError, Reassembler, Service,
    Status,
};
