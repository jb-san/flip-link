#![no_main]
#![no_std]

extern crate alloc;
extern crate flipperzero_alloc;
extern crate flipperzero_rt;

mod ir_instrument;
mod registry;
mod sys_instrument;

use core::ffi::{CStr, c_void};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use flip_proto::{DecodeResult, MsgType, decode, encode};
use flipperzero_rt::{entry, manifest};
use flipperzero_sys as sys;

manifest!(
    name = "flip-link",
    app_version = 1,
    has_icon = false,
    stack_size = 8192,
);

entry!(main);

/// Interface 1 = our binary channel; interface 0 stays the CLI/RPC.
const AGENT_IF: u8 = 1;
const RX_STREAM_CAP: usize = 1024;
const CHUNK: usize = 64;
const ENC_CAP: usize = 1100;
/// ~5 min of idle 20ms receive timeouts, then restore USB and exit.
const MAX_IDLE: u32 = 15_000;
/// Largest accepted inbound frame (header+payload+crc). Guards the heap
/// accumulator: anything bigger is treated as garbage and dropped.
const MAX_FRAME: usize = 16 * 1024;

/// RX stream buffer handle, shared from the USB ISR callback to the main loop.
static RX_STREAM: AtomicPtr<sys::FuriStreamBuffer> = AtomicPtr::new(core::ptr::null_mut());
/// Cleared by the GUI input callback (Back press) to end the main loop.
static RUNNING: AtomicBool = AtomicBool::new(true);

/// USB ISR: drain the agent CDC endpoint into the RX stream buffer. Must be
/// quick and ISR-safe — it only reads the endpoint and hands bytes to the buffer
/// (the main loop does the framing/echo). This mirrors the proven C prototype.
unsafe extern "C" fn rx_callback(_ctx: *mut c_void) {
    let sb = RX_STREAM.load(Ordering::Acquire);
    if sb.is_null() {
        return;
    }
    let mut buf = [0u8; CHUNK];
    let n = unsafe { sys::furi_hal_cdc_receive(AGENT_IF, buf.as_mut_ptr(), CHUNK as u16) };
    if n > 0 {
        unsafe { sys::furi_stream_buffer_send(sb, buf.as_ptr() as *const c_void, n as usize, 0) };
    }
}

/// USB ISR: TX endpoint complete. Small frames need no flow control; no-op.
unsafe extern "C" fn tx_callback(_ctx: *mut c_void) {}

/// CDC callbacks registered for interface 1 BEFORE switching to dual-CDC, so the
/// USB ISR always has valid pointers to call on enumeration/data. Leaving these
/// unset makes the ISR jump to stale/null pointers -> HardFault (device freeze).
/// state/ctrl_line/config are None — the firmware null-checks them (as the C
/// prototype relies on).
static CDC_CALLBACKS: sys::CdcCallbacks = sys::CdcCallbacks {
    tx_ep_callback: Some(tx_callback),
    rx_ep_callback: Some(rx_callback),
    state_callback: None,
    ctrl_line_callback: None,
    config_callback: None,
};

/// GUI draw callback: a simple status screen so the app is visibly alive.
unsafe extern "C" fn draw_callback(canvas: *mut sys::Canvas, _ctx: *mut c_void) {
    unsafe {
        sys::canvas_clear(canvas);
        sys::canvas_set_font(canvas, sys::FontPrimary);
        sys::canvas_draw_str(canvas, 2, 14, c"flip-link".as_ptr());
        sys::canvas_set_font(canvas, sys::FontSecondary);
        sys::canvas_draw_str(canvas, 2, 30, c"USB agent running".as_ptr());
        sys::canvas_draw_str(canvas, 2, 42, c"interface 1 ready".as_ptr());
        sys::canvas_draw_str(canvas, 2, 60, c"Press Back to exit".as_ptr());
    }
}

/// GUI input callback (runs on the GUI thread): exit on a short Back press.
unsafe extern "C" fn input_callback(event: *mut sys::InputEvent, _ctx: *mut c_void) {
    if event.is_null() {
        return;
    }
    let ev = unsafe { &*event };
    if ev.type_ == sys::InputTypeShort && ev.key == sys::InputKeyBack {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Send all bytes on the agent CDC interface in endpoint-sized chunks.
fn cdc_send_all(bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        let end = core::cmp::min(off + CHUNK, bytes.len());
        let chunk = &bytes[off..end];
        unsafe { sys::furi_hal_cdc_send(AGENT_IF, chunk.as_ptr() as *mut u8, chunk.len() as u16) };
        off = end;
    }
}

/// Encode a control body and send it as a frame of `typ` with `seq`.
/// The frame buffer is heap-allocated (not a large stack array) — this path runs
/// nested under main's buffers + the minicbor decode, so a stack array here can
/// overflow the FAP stack (MPU fault).
fn send_msg<T: minicbor::Encode<()>>(typ: MsgType, seq: u16, body: &T) {
    let payload = flip_proto::messages::to_payload(body);
    let mut frame = alloc::vec![0u8; flip_proto::HEADER_SIZE + payload.len() + 2];
    if let Some(n) = encode(typ, 0, seq, &payload, &mut frame) {
        cdc_send_all(&frame[..n]);
    }
}

/// Send a STREAM_START frame with a `{format}` body.
fn send_stream_start(seq: u16, format: &str) {
    send_msg(
        MsgType::StreamStart,
        seq,
        &flip_proto::StreamStart {
            format: alloc::string::String::from(format),
        },
    );
}

/// Send a STREAM_DATA frame with a raw payload (no CBOR).
fn send_stream_data(seq: u16, raw: &[u8]) {
    let mut frame = alloc::vec![0u8; flip_proto::HEADER_SIZE + raw.len() + 2];
    if let Some(n) = encode(MsgType::StreamData, 0, seq, raw, &mut frame) {
        cdc_send_all(&frame[..n]);
    }
}

/// Send the final STREAM_STOP frame with a `{dropped}` body.
fn send_stream_stop(seq: u16, dropped: u32) {
    send_msg(MsgType::StreamStop, seq, &flip_proto::StreamStop { dropped });
}

/// Send an ERROR frame with a `{code,message}` body.
fn send_error(seq: u16, code: u32, message: &str) {
    send_msg(
        MsgType::Error,
        seq,
        &flip_proto::AgentError {
            code,
            message: alloc::string::String::from(message),
        },
    );
}

/// Handle one decoded control frame (HELLO/REQ/STREAM_STOP); PING is handled
/// inline in the drain loop. Unknown/other types are ignored.
fn handle_frame(typ: MsgType, seq: u16, payload: &[u8]) {
    use flip_proto::messages::{Req, Resp, from_payload};
    match typ {
        MsgType::Hello => {
            let caps = registry::build_caps();
            send_msg(MsgType::Caps, seq, &caps);
        }
        MsgType::Req => match from_payload::<Req>(payload) {
            Ok(req) => {
                // ir.capture is a streaming op — it starts a STREAM, not a RESP.
                if req.instrument == "ir" && req.opcode == "capture" {
                    ir_instrument::start_capture(seq, send_stream_start, |s: u16, c: u32, m: &str| {
                        send_error(s, c, m)
                    });
                } else {
                    match registry::dispatch(&req.instrument, &req.opcode, &req.params) {
                        Ok(result) => send_msg(MsgType::Resp, seq, &Resp { ok: true, result }),
                        Err((code, message)) => send_error(seq, code, &message),
                    }
                }
            }
            Err(_) => send_error(seq, flip_proto::messages::ERR_BAD_PARAMS, "bad REQ body"),
        },
        // Client asks to end the active capture.
        MsgType::StreamStop => {
            ir_instrument::stop_capture(send_stream_data, send_stream_stop);
        }
        _ => {}
    }
}

fn main(_args: Option<&CStr>) -> i32 {
    // GUI: a fullscreen viewport so the screen shows the app is alive (replaces
    // the loader hourglass) and Back exits cleanly.
    let gui = unsafe { sys::furi_record_open(c"gui".as_ptr()) as *mut sys::Gui };
    let view_port = unsafe { sys::view_port_alloc() };
    unsafe {
        sys::view_port_draw_callback_set(view_port, Some(draw_callback), core::ptr::null_mut());
        sys::view_port_input_callback_set(view_port, Some(input_callback), core::ptr::null_mut());
        sys::gui_add_view_port(gui, view_port, sys::GuiLayerFullscreen);
        sys::view_port_update(view_port);
    }

    // Allocate the ISR->main RX stream buffer and publish its handle first, so a
    // callback firing during the config switch always sees a valid buffer.
    let rx_stream = unsafe { sys::furi_stream_buffer_alloc(RX_STREAM_CAP, 1) };
    RX_STREAM.store(rx_stream, Ordering::Release);

    // Register CDC callbacks for interface 1 BEFORE switching config, then unlock
    // and switch to dual-CDC. Verify the switch; if it failed, tear down cleanly.
    let prev = unsafe { sys::furi_hal_usb_get_config() };
    let switched = unsafe {
        sys::furi_hal_cdc_set_callbacks(
            AGENT_IF,
            &raw const CDC_CALLBACKS as *mut sys::CdcCallbacks,
            core::ptr::null_mut(),
        );
        sys::furi_hal_usb_unlock();
        sys::furi_hal_usb_set_config(&raw mut sys::usb_cdc_dual, core::ptr::null_mut())
    };
    if !switched {
        usb_teardown(prev, rx_stream);
        gui_teardown(gui, view_port);
        return -1;
    }

    let mut acc: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    let mut chunk = [0u8; CHUNK];
    let mut enc = [0u8; ENC_CAP];
    let recv_timeout = unsafe { sys::furi_ms_to_ticks(20) };
    let mut idle: u32 = 0;

    while RUNNING.load(Ordering::Acquire) && idle < MAX_IDLE {
        let got = unsafe {
            sys::furi_stream_buffer_receive(
                rx_stream,
                chunk.as_mut_ptr() as *mut c_void,
                CHUNK,
                recv_timeout,
            )
        };
        if got == 0 {
            idle += 1;
            continue;
        }
        idle = 0;

        // Append to the heap accumulator; drop everything if it grows past a
        // sane max (garbage/oversized frame) so the resync loop can't wedge.
        acc.extend_from_slice(&chunk[..got]);
        if acc.len() > MAX_FRAME {
            acc.clear();
            continue;
        }

        // Drain whole frames, echoing PONG for each PING.
        loop {
            let used;
            let mut send_len = 0usize;
            match decode(&acc) {
                DecodeResult::Frame(f, consumed) => {
                    match f.typ {
                        MsgType::Ping => {
                            if let Some(en) = encode(MsgType::Pong, 0, f.seq, f.payload, &mut enc) {
                                send_len = en;
                            }
                        }
                        _ => handle_frame(f.typ, f.seq, f.payload),
                    }
                    used = consumed;
                }
                DecodeResult::NeedMore => break,
                DecodeResult::Resync => {
                    if acc.is_empty() {
                        break;
                    }
                    acc.drain(0..1);
                    continue;
                }
            }
            if send_len > 0 {
                cdc_send_all(&enc[..send_len]);
            }
            acc.drain(0..used);
        }

        // Stream captured IR samples out while a capture is active.
        if ir_instrument::capture_active() {
            ir_instrument::drain_capture(send_stream_data);
        }
    }

    // If the user pressed Back mid-capture, finish the stream cleanly.
    ir_instrument::stop_capture(send_stream_data, send_stream_stop);

    usb_teardown(prev, rx_stream);
    gui_teardown(gui, view_port);
    0
}

/// Clear CDC callbacks, restore the previous USB config, and free the RX buffer.
/// Callbacks are cleared first so no ISR can touch the buffer after it is freed.
fn usb_teardown(prev: *mut sys::FuriHalUsbInterface, rx_stream: *mut sys::FuriStreamBuffer) {
    unsafe {
        sys::furi_hal_cdc_set_callbacks(AGENT_IF, core::ptr::null_mut(), core::ptr::null_mut());
        sys::furi_hal_usb_unlock();
        sys::furi_hal_usb_set_config(prev, core::ptr::null_mut());
    }
    RX_STREAM.store(core::ptr::null_mut(), Ordering::Release);
    unsafe { sys::furi_stream_buffer_free(rx_stream) };
}

/// Remove and free the viewport, and close the GUI record.
fn gui_teardown(gui: *mut sys::Gui, view_port: *mut sys::ViewPort) {
    unsafe {
        sys::gui_remove_view_port(gui, view_port);
        sys::view_port_free(view_port);
        sys::furi_record_close(c"gui".as_ptr());
    }
}
