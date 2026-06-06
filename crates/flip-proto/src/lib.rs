//! flip-link wire contract. `no_std` so it compiles into the FAP and the host.
//! The frame codec is always `no_std`/alloc-free; the CBOR control messages
//! (the `value`/`messages` modules) require the `alloc` feature.
#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod crc16;
pub mod frame;

#[cfg(feature = "alloc")]
pub mod value;

pub use crc16::crc16_ccitt_false;
pub use frame::{decode, encode, DecodeResult, Frame, MsgType, FRAME_MAGIC, HEADER_SIZE, MAX_PAYLOAD};

#[cfg(feature = "alloc")]
pub use value::Value;
