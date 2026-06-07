use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU32, Ordering};

use flip_proto::Value;
use flip_proto::messages::{ERR_BAD_PARAMS, ERR_BUSY, ERR_INTERNAL, ERR_OVERSIZED};
use flipperzero_sys as sys;

const MAX_EDGES: usize = 4096;
const CAPTURE_CAP: usize = 8192;
const EDGE_RECORD_SIZE: usize = 5;
const MAX_LINK_PROBE_BYTES: usize = 60;
const MIN_LINK_PROBE_TIMEOUT_MS: u32 = 100;
const MAX_LINK_PROBE_TIMEOUT_MS: u32 = 5_000;
const DEFAULT_LINK_PROBE_TIMEOUT_MS: u32 = 500;

static CAPTURE_STREAM: AtomicPtr<sys::FuriStreamBuffer> = AtomicPtr::new(core::ptr::null_mut());
static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CAPTURE_DEVICE: AtomicPtr<sys::SubGhzDevice> = AtomicPtr::new(core::ptr::null_mut());
static CAPTURE_SEQ: AtomicU16 = AtomicU16::new(0);
static CAPTURE_DROPPED: AtomicU32 = AtomicU32::new(0);
static TX_PTR: AtomicPtr<TxEdge> = AtomicPtr::new(core::ptr::null_mut());
static TX_LEN: AtomicU32 = AtomicU32::new(0);
static TX_POS: AtomicU32 = AtomicU32::new(0);
static TX_REPEAT: AtomicU32 = AtomicU32::new(1);
static TX_REPEAT_POS: AtomicU32 = AtomicU32::new(0);
static LINK_PROBE_CALLBACKS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
struct TxEdge {
    level: bool,
    duration_us: u32,
}

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::U64(n) => Some(*n),
        Value::I64(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

fn required_frequency(params: &Value) -> Result<u32, (u32, String)> {
    params
        .get("frequency")
        .and_then(as_u64)
        .map(|n| n as u32)
        .ok_or_else(|| (ERR_BAD_PARAMS, "frequency required".to_string()))
}

fn required_preset(params: &Value) -> Result<sys::FuriHalSubGhzPreset, (u32, String)> {
    match params.get("preset") {
        Some(Value::Text(name)) => preset_by_name(name),
        _ => Err((ERR_BAD_PARAMS, "preset required".to_string())),
    }
}

fn preset_by_name(name: &str) -> Result<sys::FuriHalSubGhzPreset, (u32, String)> {
    match name {
        "ook270" => Ok(sys::FuriHalSubGhzPresetOok270Async),
        "ook650" => Ok(sys::FuriHalSubGhzPresetOok650Async),
        "2fsk_dev238" => Ok(sys::FuriHalSubGhzPreset2FSKDev238Async),
        "2fsk_dev476" => Ok(sys::FuriHalSubGhzPreset2FSKDev476Async),
        "msk99_97" => Ok(sys::FuriHalSubGhzPresetMSK99_97KbAsync),
        "gfsk9_99" => Ok(sys::FuriHalSubGhzPresetGFSK9_99KbAsync),
        _ => Err((ERR_BAD_PARAMS, "unknown subghz preset".to_string())),
    }
}

unsafe extern "C" fn rx_capture_isr(level: bool, duration: u32, _context: *mut core::ffi::c_void) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() || duration == 0 {
        return;
    }

    let mut bytes = [0u8; EDGE_RECORD_SIZE];
    bytes[0] = if level { 1 } else { 0 };
    bytes[1..5].copy_from_slice(&duration.to_le_bytes());
    let sent = unsafe {
        sys::furi_stream_buffer_send(
            sb,
            bytes.as_ptr() as *const core::ffi::c_void,
            EDGE_RECORD_SIZE,
            0,
        )
    };
    if sent != EDGE_RECORD_SIZE {
        CAPTURE_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe extern "C" fn link_probe_have_read(_context: *mut core::ffi::c_void) {
    LINK_PROBE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

fn ensure_capture_buffer() -> *mut sys::FuriStreamBuffer {
    let existing = CAPTURE_STREAM.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let sb = unsafe { sys::furi_stream_buffer_alloc(CAPTURE_CAP, EDGE_RECORD_SIZE) };
    CAPTURE_STREAM.store(sb, Ordering::Release);
    sb
}

fn internal_device() -> *const sys::SubGhzDevice {
    unsafe {
        sys::subghz_devices_init();
        sys::subghz_devices_get_by_name(c"cc1101_int".as_ptr())
    }
}

fn parse_edges(params: &Value) -> Result<Vec<TxEdge>, (u32, String)> {
    let array = match params.get("edges") {
        Some(Value::Array(array)) => array,
        _ => return Err((ERR_BAD_PARAMS, "edges array required".to_string())),
    };
    if array.is_empty() {
        return Err((ERR_BAD_PARAMS, "edges empty".to_string()));
    }
    if array.len() > MAX_EDGES {
        return Err((ERR_OVERSIZED, "too many subghz edges".to_string()));
    }

    let mut out = Vec::new();
    for item in array {
        let level = match item.get("level") {
            Some(Value::Bool(level)) => *level,
            _ => return Err((ERR_BAD_PARAMS, "edge level required".to_string())),
        };
        let duration_us = match item.get("duration_us").and_then(as_u64) {
            Some(duration_us) if duration_us > 0 && duration_us <= 0x3fff_ffff => {
                duration_us as u32
            }
            _ => return Err((ERR_BAD_PARAMS, "edge duration_us out of range".to_string())),
        };
        out.push(TxEdge { level, duration_us });
    }
    Ok(out)
}

fn repeat_count(params: &Value) -> Result<u32, (u32, String)> {
    let repeat = params.get("repeat").and_then(as_u64).unwrap_or(1);
    if repeat == 0 || repeat > 100 {
        return Err((ERR_BAD_PARAMS, "repeat out of range".to_string()));
    }
    Ok(repeat as u32)
}

fn probe_payload(params: &Value) -> Result<Vec<u8>, (u32, String)> {
    let bytes = match params.get("payload") {
        Some(Value::Bytes(bytes)) => bytes,
        _ => return Err((ERR_BAD_PARAMS, "payload bytes required".to_string())),
    };
    if bytes.is_empty() {
        return Err((ERR_BAD_PARAMS, "payload empty".to_string()));
    }
    if bytes.len() > MAX_LINK_PROBE_BYTES {
        return Err((ERR_OVERSIZED, "payload too large".to_string()));
    }
    Ok(bytes.clone())
}

fn probe_timeout_ms(params: &Value) -> Result<u32, (u32, String)> {
    let timeout = params
        .get("timeout_ms")
        .and_then(as_u64)
        .unwrap_or(DEFAULT_LINK_PROBE_TIMEOUT_MS as u64);
    if timeout < MIN_LINK_PROBE_TIMEOUT_MS as u64 || timeout > MAX_LINK_PROBE_TIMEOUT_MS as u64 {
        return Err((ERR_BAD_PARAMS, "timeout_ms out of range".to_string()));
    }
    Ok(timeout as u32)
}

fn level_duration(level: bool, duration_us: u32) -> sys::LevelDuration {
    sys::LevelDuration {
        _bitfield_align_1: [],
        _bitfield_1: sys::LevelDuration::new_bitfield_1(duration_us, if level { 2 } else { 1 }),
    }
}

fn level_duration_reset() -> sys::LevelDuration {
    sys::LevelDuration {
        _bitfield_align_1: [],
        _bitfield_1: sys::LevelDuration::new_bitfield_1(0, 0),
    }
}

unsafe extern "C" fn tx_yield(_context: *mut core::ffi::c_void) -> sys::LevelDuration {
    let ptr = TX_PTR.load(Ordering::Acquire);
    let len = TX_LEN.load(Ordering::Acquire);
    if ptr.is_null() || len == 0 {
        return level_duration_reset();
    }

    let pos = TX_POS.load(Ordering::Acquire);
    let repeat_pos = TX_REPEAT_POS.load(Ordering::Acquire);
    let repeat = TX_REPEAT.load(Ordering::Acquire);
    if repeat_pos >= repeat {
        return level_duration_reset();
    }

    let edge = unsafe { *ptr.add(pos as usize) };
    let mut next_pos = pos + 1;
    let mut next_repeat_pos = repeat_pos;
    if next_pos >= len {
        next_pos = 0;
        next_repeat_pos += 1;
    }
    TX_POS.store(next_pos, Ordering::Release);
    TX_REPEAT_POS.store(next_repeat_pos, Ordering::Release);
    level_duration(edge.level, edge.duration_us)
}

pub fn transmit(params: &Value) -> Result<Value, (u32, String)> {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) || !TX_PTR.load(Ordering::Acquire).is_null() {
        return Err((ERR_BUSY, "subghz busy".to_string()));
    }

    let frequency = required_frequency(params)?;
    let preset = required_preset(params)?;
    let edges = parse_edges(params)?;
    let repeat = repeat_count(params)?;
    let device = internal_device();
    if device.is_null() {
        return Err((
            ERR_INTERNAL,
            "subghz internal device unavailable".to_string(),
        ));
    }
    if unsafe { !sys::subghz_devices_is_frequency_valid(device, frequency) } {
        return Err((ERR_BAD_PARAMS, "invalid subghz frequency".to_string()));
    }
    if unsafe { !sys::subghz_devices_begin(device) } {
        return Err((ERR_BUSY, "subghz device unavailable".to_string()));
    }

    TX_PTR.store(edges.as_ptr() as *mut TxEdge, Ordering::Release);
    TX_LEN.store(edges.len() as u32, Ordering::Release);
    TX_POS.store(0, Ordering::Release);
    TX_REPEAT.store(repeat, Ordering::Release);
    TX_REPEAT_POS.store(0, Ordering::Release);

    let allowed = unsafe {
        sys::subghz_devices_reset(device);
        sys::subghz_devices_load_preset(device, preset, core::ptr::null_mut());
        sys::subghz_devices_set_frequency(device, frequency);
        sys::subghz_devices_flush_tx(device);
        sys::furi_hal_subghz_start_async_tx(Some(tx_yield), core::ptr::null_mut())
    };
    if !allowed {
        TX_PTR.store(core::ptr::null_mut(), Ordering::Release);
        unsafe {
            sys::subghz_devices_idle(device);
            sys::subghz_devices_sleep(device);
            sys::subghz_devices_end(device);
        }
        return Err((
            ERR_BAD_PARAMS,
            "subghz transmit not allowed on this frequency".to_string(),
        ));
    }

    let total_us = edges
        .iter()
        .fold(0u64, |acc, edge| {
            acc.saturating_add(edge.duration_us as u64)
        })
        .saturating_mul(repeat as u64);
    let timeout_ms = (total_us / 1000).saturating_add(1000).min(60_000) as u32;
    let mut waited_ms = 0u32;
    while unsafe { !sys::furi_hal_subghz_is_async_tx_complete() } && waited_ms < timeout_ms {
        unsafe { sys::furi_delay_ms(1) };
        waited_ms += 1;
    }

    unsafe {
        sys::furi_hal_subghz_stop_async_tx();
        sys::subghz_devices_idle(device);
        sys::subghz_devices_sleep(device);
        sys::subghz_devices_end(device);
    }
    TX_PTR.store(core::ptr::null_mut(), Ordering::Release);
    if waited_ms >= timeout_ms {
        return Err((ERR_INTERNAL, "subghz transmit timeout".to_string()));
    }

    Ok(Value::Map(alloc::vec![(
        "sent".to_string(),
        Value::U64((edges.len() as u64).saturating_mul(repeat as u64)),
    )]))
}

pub fn link_probe(params: &Value) -> Result<Value, (u32, String)> {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) || !TX_PTR.load(Ordering::Acquire).is_null() {
        return Err((ERR_BUSY, "subghz busy".to_string()));
    }

    let frequency = required_frequency(params)?;
    let mut payload = probe_payload(params)?;
    let timeout_ms = probe_timeout_ms(params)?;
    let device = internal_device();
    if device.is_null() {
        return Err((
            ERR_INTERNAL,
            "subghz internal device unavailable".to_string(),
        ));
    }
    if unsafe { !sys::subghz_devices_is_frequency_valid(device, frequency) } {
        return Err((ERR_BAD_PARAMS, "invalid subghz frequency".to_string()));
    }
    if unsafe { !sys::furi_hal_region_is_frequency_allowed(frequency) } {
        return Err((
            ERR_BAD_PARAMS,
            "subghz link probe not allowed on this frequency".to_string(),
        ));
    }

    let worker = unsafe { sys::subghz_tx_rx_worker_alloc() };
    if worker.is_null() {
        return Err((ERR_INTERNAL, "subghz worker allocation failed".to_string()));
    }

    LINK_PROBE_CALLBACKS.store(0, Ordering::Release);
    unsafe {
        sys::subghz_tx_rx_worker_set_callback_have_read(
            worker,
            Some(link_probe_have_read),
            worker as *mut core::ffi::c_void,
        );
    }

    let started = unsafe { sys::subghz_tx_rx_worker_start(worker, device, frequency) };
    if !started {
        unsafe { sys::subghz_tx_rx_worker_free(worker) };
        return Err((ERR_BUSY, "subghz link worker unavailable".to_string()));
    }

    let wrote =
        unsafe { sys::subghz_tx_rx_worker_write(worker, payload.as_mut_ptr(), payload.len()) };
    let mut read_total = 0u64;
    let mut rx_preview = Vec::new();
    let mut waited_ms = 0u32;

    while waited_ms < timeout_ms {
        let available = unsafe { sys::subghz_tx_rx_worker_available(worker) };
        if available > 0 {
            let mut buf = [0u8; MAX_LINK_PROBE_BYTES];
            let want = core::cmp::min(available, buf.len());
            let got = unsafe { sys::subghz_tx_rx_worker_read(worker, buf.as_mut_ptr(), want) };
            read_total = read_total.saturating_add(got as u64);
            let room = MAX_LINK_PROBE_BYTES.saturating_sub(rx_preview.len());
            if room > 0 {
                let take = core::cmp::min(room, got);
                rx_preview.extend_from_slice(&buf[..take]);
            }
        }
        unsafe { sys::furi_delay_ms(1) };
        waited_ms += 1;
    }

    unsafe {
        sys::subghz_tx_rx_worker_stop(worker);
        sys::subghz_tx_rx_worker_free(worker);
    }

    if !wrote {
        return Err((ERR_INTERNAL, "subghz link worker write failed".to_string()));
    }

    Ok(Value::Map(alloc::vec![
        ("written".to_string(), Value::U64(payload.len() as u64)),
        ("read".to_string(), Value::U64(read_total)),
        (
            "callbacks".to_string(),
            Value::U64(LINK_PROBE_CALLBACKS.load(Ordering::Acquire) as u64),
        ),
        ("rx_preview".to_string(), Value::Bytes(rx_preview)),
    ]))
}

pub fn capture_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Acquire)
}

pub fn start_capture(
    seq: u16,
    params: &Value,
    send_start: impl FnOnce(u16, &str),
    send_error: impl FnOnce(u16, u32, &str),
) {
    if CAPTURE_ACTIVE.load(Ordering::Acquire) {
        send_error(seq, ERR_BUSY, "subghz busy");
        return;
    }

    let frequency = match required_frequency(params) {
        Ok(frequency) => frequency,
        Err((code, message)) => {
            send_error(seq, code, &message);
            return;
        }
    };
    let preset = match required_preset(params) {
        Ok(preset) => preset,
        Err((code, message)) => {
            send_error(seq, code, &message);
            return;
        }
    };
    let device = internal_device();
    if device.is_null() {
        send_error(seq, ERR_INTERNAL, "subghz internal device unavailable");
        return;
    }
    if unsafe { !sys::subghz_devices_is_frequency_valid(device, frequency) } {
        send_error(seq, ERR_BAD_PARAMS, "invalid subghz frequency");
        return;
    }
    if unsafe { !sys::subghz_devices_begin(device) } {
        send_error(seq, ERR_BUSY, "subghz device unavailable");
        return;
    }

    let sb = ensure_capture_buffer();
    if sb.is_null() {
        unsafe { sys::subghz_devices_end(device) };
        send_error(seq, ERR_INTERNAL, "no subghz capture buffer");
        return;
    }
    let mut scratch = [0u8; 128];
    while unsafe {
        sys::furi_stream_buffer_receive(
            sb,
            scratch.as_mut_ptr() as *mut core::ffi::c_void,
            scratch.len(),
            0,
        )
    } > 0
    {}

    CAPTURE_DROPPED.store(0, Ordering::Release);
    CAPTURE_SEQ.store(seq, Ordering::Release);
    CAPTURE_DEVICE.store(device as *mut sys::SubGhzDevice, Ordering::Release);
    CAPTURE_ACTIVE.store(true, Ordering::Release);

    unsafe {
        sys::subghz_devices_reset(device);
        sys::subghz_devices_load_preset(device, preset, core::ptr::null_mut());
        sys::subghz_devices_set_frequency(device, frequency);
        sys::subghz_devices_set_rx(device);
        sys::subghz_devices_flush_rx(device);
        sys::furi_hal_subghz_start_async_rx(Some(rx_capture_isr), core::ptr::null_mut());
    }
    send_start(
        seq,
        flip_proto::messages::STREAM_FORMAT_SUBGHZ_LEVEL_DURATION_V1,
    );
}

pub fn drain_capture(send_data: impl FnOnce(u16, &[u8])) {
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if sb.is_null() || !CAPTURE_ACTIVE.load(Ordering::Acquire) {
        return;
    }

    let mut batch = [0u8; 250];
    let got = unsafe {
        sys::furi_stream_buffer_receive(
            sb,
            batch.as_mut_ptr() as *mut core::ffi::c_void,
            batch.len(),
            0,
        )
    };
    let whole = got - (got % EDGE_RECORD_SIZE);
    if whole >= EDGE_RECORD_SIZE {
        send_data(CAPTURE_SEQ.load(Ordering::Acquire), &batch[..whole]);
    }
}

pub fn stop_capture(send_data: impl Fn(u16, &[u8]), send_stop: impl FnOnce(u16, u32)) {
    if !CAPTURE_ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }

    unsafe {
        sys::furi_hal_subghz_stop_async_rx();
    }
    let device = CAPTURE_DEVICE.swap(core::ptr::null_mut(), Ordering::AcqRel);
    if !device.is_null() {
        unsafe {
            sys::subghz_devices_idle(device);
            sys::subghz_devices_sleep(device);
            sys::subghz_devices_end(device);
        }
    }

    let seq = CAPTURE_SEQ.load(Ordering::Acquire);
    let sb = CAPTURE_STREAM.load(Ordering::Acquire);
    if !sb.is_null() {
        loop {
            let mut batch = [0u8; 250];
            let got = unsafe {
                sys::furi_stream_buffer_receive(
                    sb,
                    batch.as_mut_ptr() as *mut core::ffi::c_void,
                    batch.len(),
                    0,
                )
            };
            let whole = got - (got % EDGE_RECORD_SIZE);
            if whole < EDGE_RECORD_SIZE {
                break;
            }
            send_data(seq, &batch[..whole]);
        }
    }
    send_stop(seq, CAPTURE_DROPPED.load(Ordering::Acquire));
}
