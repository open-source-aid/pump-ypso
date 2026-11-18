#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod counters;
pub mod protocol;
#[cfg(feature = "sim")]
pub mod sim;

pub use counters::{CounterError, CounterState, Counters, EpochChange};
pub use protocol::{
    AttError, Characteristic, Crypto, Frames, Opened, ProtoError, PumpError, Reassembler, Service,
    Status,
};
