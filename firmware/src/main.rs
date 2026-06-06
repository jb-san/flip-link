#![no_main]
#![no_std]

extern crate flipperzero_rt;

use core::ffi::CStr;
use flipperzero::println;
use flipperzero_rt::{entry, manifest};

manifest!(
    name = "flip-link",
    app_version = 1,
    has_icon = false,
);

entry!(main);

fn main(_args: Option<&CStr>) -> i32 {
    println!("flip-link toolchain OK");
    0
}
