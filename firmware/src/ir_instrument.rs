//! IR instrument — transmit only (Slice 1b). Capture is Slice 1c.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use flip_proto::messages::{ERR_BAD_PARAMS, ERR_BUSY, ERR_OVERSIZED};
use flip_proto::Value;
use flipperzero_sys as sys;

const DEFAULT_FREQ: u32 = 38_000;
const DEFAULT_DUTY_PERMILLE: u32 = 330;
const MAX_EDGES: usize = 1024;

/// Timings shared with the TX get-data ISR. Valid only between
/// async_tx_start and wait_termination; the owning Vec lives on the handler
/// stack for that whole window.
static TX_PTR: AtomicPtr<u32> = AtomicPtr::new(core::ptr::null_mut());
static TX_LEN: AtomicUsize = AtomicUsize::new(0);
static TX_POS: AtomicUsize = AtomicUsize::new(0);

/// IR TX get-data ISR: feed the next edge. Odd/even position = mark/space
/// (first timing is a mark, LED on).
unsafe extern "C" fn tx_get_data(
    _ctx: *mut core::ffi::c_void,
    duration: *mut u32,
    level: *mut bool,
) -> sys::FuriHalInfraredTxGetDataState {
    let ptr = TX_PTR.load(Ordering::Acquire);
    let len = TX_LEN.load(Ordering::Acquire);
    let pos = TX_POS.load(Ordering::Acquire);
    if ptr.is_null() || pos >= len {
        return sys::FuriHalInfraredTxGetDataStateLastDone;
    }
    unsafe {
        *duration = *ptr.add(pos);
        *level = (pos % 2) == 0;
    }
    TX_POS.store(pos + 1, Ordering::Release);
    if pos + 1 >= len {
        sys::FuriHalInfraredTxGetDataStateLastDone
    } else {
        sys::FuriHalInfraredTxGetDataStateOk
    }
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::U64(n) => Some(*n),
        Value::I64(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

/// `ir.transmit` — params { frequency?:u, duty_permille?:u, timings:[u,...] }.
pub fn transmit(params: &Value) -> Result<Value, (u32, String)> {
    let freq = params
        .get("frequency")
        .and_then(as_u64)
        .unwrap_or(DEFAULT_FREQ as u64) as u32;
    let duty_permille = params
        .get("duty_permille")
        .and_then(as_u64)
        .unwrap_or(DEFAULT_DUTY_PERMILLE as u64) as u32;

    let timings: Vec<u32> = match params.get("timings") {
        Some(Value::Array(a)) => a.iter().filter_map(as_u64).map(|v| v as u32).collect(),
        _ => return Err((ERR_BAD_PARAMS, "timings array required".to_string())),
    };
    if timings.is_empty() {
        return Err((ERR_BAD_PARAMS, "timings empty".to_string()));
    }
    if timings.len() > MAX_EDGES {
        return Err((ERR_OVERSIZED, "too many timings".to_string()));
    }
    if unsafe { sys::furi_hal_infrared_is_busy() } {
        return Err((ERR_BUSY, "ir busy".to_string()));
    }

    // Publish timings to the ISR, then drive the async transmission. `timings`
    // stays alive on this stack frame until wait_termination returns.
    TX_PTR.store(timings.as_ptr() as *mut u32, Ordering::Release);
    TX_LEN.store(timings.len(), Ordering::Release);
    TX_POS.store(0, Ordering::Release);

    let duty = duty_permille as f32 / 1000.0;
    unsafe {
        sys::furi_hal_infrared_async_tx_set_data_isr_callback(
            Some(tx_get_data),
            core::ptr::null_mut(),
        );
        sys::furi_hal_infrared_async_tx_start(freq, duty);
        sys::furi_hal_infrared_async_tx_wait_termination();
        sys::furi_hal_infrared_async_tx_stop();
    }
    TX_PTR.store(core::ptr::null_mut(), Ordering::Release);

    Ok(Value::Map(vec![(
        "sent".to_string(),
        Value::U64(timings.len() as u64),
    )]))
}
