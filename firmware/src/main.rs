#![no_main]
#![no_std]

extern crate flipperzero_rt;

use core::ffi::CStr;
use flip_proto::{DecodeResult, MsgType, decode, encode};
use flipperzero_rt::{entry, manifest};
use flipperzero_sys as sys;

manifest!(name = "flip-link", app_version = 1, has_icon = false,);

entry!(main);

/// Interface 1 = our binary channel; interface 0 stays the CLI/RPC.
const AGENT_IF: u8 = 1;
const ACC_CAP: usize = 1024;
const RX_CAP: usize = 256;
const ENC_CAP: usize = 1100;
/// Time-box the spike: idle ticks * 2ms ≈ 5 minutes, then restore USB and exit.
const MAX_IDLE_TICKS: u32 = 150_000;

/// Send all bytes on the agent CDC interface in 64-byte (endpoint-sized) chunks.
fn cdc_send_all(bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        let end = core::cmp::min(off + 64, bytes.len());
        let chunk = &bytes[off..end];
        unsafe {
            sys::furi_hal_cdc_send(AGENT_IF, chunk.as_ptr() as *mut u8, chunk.len() as u16);
        }
        off = end;
    }
}

fn main(_args: Option<&CStr>) -> i32 {
    // Save current USB config, switch to dual-CDC (exposes interface 1 = ours).
    let prev = unsafe { sys::furi_hal_usb_get_config() };
    unsafe {
        sys::furi_hal_usb_set_config(&raw mut sys::usb_cdc_dual, core::ptr::null_mut());
    }

    let mut acc = [0u8; ACC_CAP];
    let mut acc_len: usize = 0;
    let mut rx = [0u8; RX_CAP];
    let mut idle_ticks: u32 = 0;

    loop {
        let got = unsafe { sys::furi_hal_cdc_receive(AGENT_IF, rx.as_mut_ptr(), RX_CAP as u16) };
        if got > 0 {
            idle_ticks = 0;
            let got = got as usize;
            // Append to the accumulator (drop overflow defensively).
            let space = ACC_CAP - acc_len;
            let n = core::cmp::min(space, got);
            acc[acc_len..acc_len + n].copy_from_slice(&rx[..n]);
            acc_len += n;

            // Drain whole frames, echoing PONG for each PING.
            loop {
                let (used, send_len, enc);
                match decode(&acc[..acc_len]) {
                    DecodeResult::Frame(f, consumed) => {
                        let mut buf = [0u8; ENC_CAP];
                        let mut sl = 0usize;
                        if f.typ == MsgType::Ping {
                            if let Some(en) = encode(MsgType::Pong, 0, f.seq, f.payload, &mut buf) {
                                sl = en;
                            }
                        }
                        used = consumed;
                        send_len = sl;
                        enc = buf;
                    }
                    DecodeResult::NeedMore => break,
                    DecodeResult::Resync => {
                        if acc_len == 0 {
                            break;
                        }
                        acc.copy_within(1..acc_len, 0);
                        acc_len -= 1;
                        continue;
                    }
                }
                if send_len > 0 {
                    cdc_send_all(&enc[..send_len]);
                }
                acc.copy_within(used..acc_len, 0);
                acc_len -= used;
            }
        } else {
            unsafe { sys::furi_delay_ms(2) };
            idle_ticks += 1;
            if idle_ticks >= MAX_IDLE_TICKS {
                break;
            }
        }
    }

    // Restore the previous USB config on exit.
    unsafe {
        sys::furi_hal_usb_set_config(prev, core::ptr::null_mut());
    }
    0
}
