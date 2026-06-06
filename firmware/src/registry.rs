//! Static instrument/opcode table, dispatch, and CAPS construction.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use flip_proto::Value;
use flip_proto::messages::{Caps, Instrument, PROTOCOL_VERSION};

use crate::sys_instrument;

struct OpcodeEntry {
    opcode: &'static str,
    handler: sys_instrument::Handler,
}

struct InstrumentEntry {
    id: &'static str,
    opcodes: &'static [OpcodeEntry],
}

static SYS_OPCODES: &[OpcodeEntry] = &[
    OpcodeEntry {
        opcode: "version",
        handler: sys_instrument::version,
    },
    OpcodeEntry {
        opcode: "echo",
        handler: sys_instrument::echo,
    },
];

static IR_OPCODES: &[OpcodeEntry] = &[OpcodeEntry {
    opcode: "transmit",
    handler: crate::ir_instrument::transmit,
}];

static INSTRUMENTS: &[InstrumentEntry] = &[
    InstrumentEntry {
        id: "sys",
        opcodes: SYS_OPCODES,
    },
    InstrumentEntry {
        id: "ir",
        opcodes: IR_OPCODES,
    },
];

/// Find a handler by instrument id + opcode. Returns None if either is unknown.
pub fn find(instrument: &str, opcode: &str) -> Option<sys_instrument::Handler> {
    let inst = INSTRUMENTS.iter().find(|i| i.id == instrument)?;
    inst.opcodes
        .iter()
        .find(|o| o.opcode == opcode)
        .map(|o| o.handler)
}

/// True if the instrument id exists (to distinguish unknown-instrument from
/// unknown-opcode errors).
pub fn has_instrument(instrument: &str) -> bool {
    INSTRUMENTS.iter().any(|i| i.id == instrument)
}

/// Build the CAPS body from the static table.
pub fn build_caps() -> Caps {
    let instruments = INSTRUMENTS
        .iter()
        .map(|i| Instrument {
            id: i.id.to_string(),
            opcodes: i
                .opcodes
                .iter()
                .map(|o| o.opcode.to_string())
                .collect::<Vec<String>>(),
        })
        .collect();
    Caps {
        protocol_version: PROTOCOL_VERSION,
        instruments,
    }
}

/// Dispatch a decoded request to its handler. Returns the result Value or an
/// (error_code, message). Unknown instrument/opcode produce the mirrored codes.
pub fn dispatch(instrument: &str, opcode: &str, params: &Value) -> Result<Value, (u32, String)> {
    match find(instrument, opcode) {
        Some(handler) => handler(params),
        None if !has_instrument(instrument) => Err((
            flip_proto::messages::ERR_UNKNOWN_INSTRUMENT,
            "unknown instrument".to_string(),
        )),
        None => Err((
            flip_proto::messages::ERR_UNKNOWN_OPCODE,
            "unknown opcode".to_string(),
        )),
    }
}
