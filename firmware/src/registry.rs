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
    streaming_opcodes: &'static [&'static str],
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

static SUBGHZ_OPCODES: &[OpcodeEntry] = &[
    OpcodeEntry {
        opcode: "transmit",
        handler: crate::subghz_instrument::transmit,
    },
    OpcodeEntry {
        opcode: "link_probe",
        handler: crate::subghz_instrument::link_probe,
    },
];

static INSTRUMENTS: &[InstrumentEntry] = &[
    InstrumentEntry {
        id: "sys",
        opcodes: SYS_OPCODES,
        streaming_opcodes: &[],
    },
    InstrumentEntry {
        id: "ir",
        opcodes: IR_OPCODES,
        streaming_opcodes: &["capture"],
    },
    InstrumentEntry {
        id: "subghz",
        opcodes: SUBGHZ_OPCODES,
        streaming_opcodes: &["capture"],
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

/// True if the instrument/opcode is advertised as a streaming operation.
pub fn is_streaming(instrument: &str, opcode: &str) -> bool {
    INSTRUMENTS
        .iter()
        .find(|i| i.id == instrument)
        .map(|i| i.streaming_opcodes.contains(&opcode))
        .unwrap_or(false)
}

/// Build the CAPS body from the static table.
pub fn build_caps() -> Caps {
    let instruments = INSTRUMENTS
        .iter()
        .map(|i| {
            let mut opcodes: Vec<String> = i.opcodes.iter().map(|o| o.opcode.to_string()).collect();
            opcodes.extend(i.streaming_opcodes.iter().map(|s| s.to_string()));
            Instrument {
                id: i.id.to_string(),
                opcodes,
            }
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
