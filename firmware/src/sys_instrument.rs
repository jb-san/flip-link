//! Hardware-free test instrument: proves the REQ/RESP control plane.

use alloc::string::{String, ToString};
use alloc::vec;
use flip_proto::Value;

/// A handler maps decoded params to a result Value, or an (error_code, message).
pub type Handler = fn(params: &Value) -> Result<Value, (u32, String)>;

/// `sys.version` — ignores params, returns protocol + firmware identity.
pub fn version(_params: &Value) -> Result<Value, (u32, String)> {
    Ok(Value::Map(vec![
        (
            "protocol".to_string(),
            Value::U64(flip_proto::messages::PROTOCOL_VERSION as u64),
        ),
        ("fw".to_string(), Value::Text("flip-link 0.1".to_string())),
    ]))
}

/// `sys.echo` — returns its params unchanged (proves params round-trip).
pub fn echo(params: &Value) -> Result<Value, (u32, String)> {
    Ok(params.clone())
}
