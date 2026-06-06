//! IR instrument — transmit only (Slice 1b). Capture is Slice 1c.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU32, AtomicUsize, Ordering};

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

    // Publish order matters: the atomics + the owning Vec are set BEFORE the
    // callback is registered and tx_start kicks off DMA, so the ISR (which only
    // fires once DMA runs) can never observe a half-published state.
    let duty = duty_permille as f32 / 1000.0;
    unsafe {
        sys::furi_hal_infrared_async_tx_set_data_isr_callback(
            Some(tx_get_data),
            core::ptr::null_mut(),
        );
        sys::furi_hal_infrared_async_tx_start(freq, duty);
        // wait_termination blocks until the signal finishes AND frees resources;
        // it is the *alternative* to _stop (calling both double-frees HAL state),
        // matching the proven C prototype which uses wait_termination alone.
        sys::furi_hal_infrared_async_tx_wait_termination();
    }
    TX_PTR.store(core::ptr::null_mut(), Ordering::Release);

    Ok(Value::Map(vec![(
        "sent".to_string(),
        Value::U64(timings.len() as u64),
    )]))
}

/// Silence (µs) after which the RX hardware reports a timeout (burst ended).
const RX_TIMEOUT_US: u32 = 150_000;
/// Capture stream buffer capacity in bytes (i32 samples).
const CAPTURE_CAP: usize = 4096;

static CAPTURE_STREAM: AtomicPtr<sys::FuriStreamBuffer> = AtomicPtr::new(core::ptr::null_mut());
static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CAPTURE_SEQ: AtomicU16 = AtomicU16::new(0);
static CAPTURE_DROPPED: AtomicU32 = AtomicU32::new(0);

/// RX capture ISR: push each edge duration as a little-endian i32 (µs) into the
/// capture buffer. `level` is implicit in ordering (host replays durations in
/// order). On buffer-full, count a drop; never block the ISR.
unsafe extern "C" fn rx_capture_isr(_ctx: *mut core::ffi::c_void, _level: bool, duration: u32) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() {
        return;
    }
    let d = duration as i32;
    let bytes = d.to_le_bytes();
    let sent = unsafe {
        sys::furi_stream_buffer_send(sb, bytes.as_ptr() as *const core::ffi::c_void, 4, 0)
    };
    if sent != 4 {
        CAPTURE_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// RX silence-timeout ISR: the host decides when to stop; nothing to do here.
unsafe extern "C" fn rx_timeout_isr(_ctx: *mut core::ffi::c_void) {}

/// True if a capture is currently streaming.
pub fn capture_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Acquire)
}

/// Allocate the capture buffer once, lazily.
fn ensure_capture_buffer() -> *mut sys::FuriStreamBuffer {
    let existing = CAPTURE_STREAM.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let sb = unsafe { sys::furi_stream_buffer_alloc(CAPTURE_CAP, 4) };
    CAPTURE_STREAM.store(sb, Ordering::Release);
    sb
}

/// Start an IR capture for request `seq`: send STREAM_START, arm RX. The caller
/// (main loop) drains via `drain_capture` and ends via `stop_capture`. On busy,
/// sends an ERROR and does not start.
pub fn start_capture(
    seq: u16,
    send_start: impl FnOnce(u16, &str),
    send_error: impl FnOnce(u16, u32, &str),
) {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) || unsafe { sys::furi_hal_infrared_is_busy() } {
        send_error(seq, ERR_BUSY, "ir busy");
        return;
    }
    let sb = ensure_capture_buffer();
    if sb.is_null() {
        send_error(seq, flip_proto::messages::ERR_INTERNAL, "no capture buffer");
        return;
    }
    // Drain any stale bytes from a previous capture.
    let mut scratch = [0u8; 64];
    while unsafe {
        sys::furi_stream_buffer_receive(sb, scratch.as_mut_ptr() as *mut core::ffi::c_void, 64, 0)
    } > 0
    {}
    CAPTURE_DROPPED.store(0, Ordering::Release);
    CAPTURE_SEQ.store(seq, Ordering::Release);
    CAPTURE_ACTIVE.store(true, Ordering::Release);

    send_start(seq, flip_proto::messages::STREAM_FORMAT_RAW_I32_US);

    unsafe {
        sys::furi_hal_infrared_async_rx_set_capture_isr_callback(
            Some(rx_capture_isr),
            core::ptr::null_mut(),
        );
        sys::furi_hal_infrared_async_rx_set_timeout_isr_callback(
            Some(rx_timeout_isr),
            core::ptr::null_mut(),
        );
        sys::furi_hal_infrared_async_rx_set_timeout(RX_TIMEOUT_US);
        sys::furi_hal_infrared_async_rx_start();
    }
}

/// Drain available whole i32 samples into one STREAM_DATA frame (non-blocking).
/// Call every main-loop iteration while `capture_active`.
pub fn drain_capture(send_data: impl FnOnce(u16, &[u8])) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() || !CAPTURE_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let mut batch = [0u8; 256];
    let got = unsafe {
        sys::furi_stream_buffer_receive(sb, batch.as_mut_ptr() as *mut core::ffi::c_void, 256, 0)
    };
    let whole = got - (got % 4);
    if whole >= 4 {
        send_data(CAPTURE_SEQ.load(Ordering::Acquire), &batch[..whole]);
    }
}

/// Stop the active capture: stop RX, flush remaining samples, send final
/// STREAM_STOP{dropped}. Safe to call when no capture is active (no-op).
pub fn stop_capture(send_data: impl Fn(u16, &[u8]), send_stop: impl FnOnce(u16, u32)) {
    if !CAPTURE_ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }
    unsafe {
        sys::furi_hal_infrared_async_rx_stop();
    }
    let seq = CAPTURE_SEQ.load(Ordering::Acquire);
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if !sb.is_null() {
        loop {
            let mut batch = [0u8; 256];
            let got = unsafe {
                sys::furi_stream_buffer_receive(
                    sb,
                    batch.as_mut_ptr() as *mut core::ffi::c_void,
                    256,
                    0,
                )
            };
            let whole = got - (got % 4);
            if whole < 4 {
                break;
            }
            send_data(seq, &batch[..whole]);
        }
    }
    send_stop(seq, CAPTURE_DROPPED.load(Ordering::Acquire));
}
