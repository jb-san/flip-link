//! flip-link wire contract. `no_std` so it compiles into the FAP and the host.
#![cfg_attr(not(test), no_std)]

pub mod crc16;
pub mod frame;

pub use crc16::crc16_ccitt_false;
pub use frame::{
    decode, encode, DecodeResult, Frame, MsgType, FRAME_MAGIC, HEADER_SIZE, MAX_PAYLOAD,
};
