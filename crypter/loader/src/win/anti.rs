//! Minimal anti-analysis: debugger checks + sandbox timeout delay.

use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Diagnostics::Debug::{
    CheckRemoteDebuggerPresent, IsDebuggerPresent,
};
use windows::Win32::System::Threading::GetCurrentProcess;

/// True if a user- or kernel-mode debugger appears attached.
pub fn detected() -> bool {
    unsafe {
        if IsDebuggerPresent().as_bool() {
            return true;
        }
        let mut remote = BOOL(0);
        if CheckRemoteDebuggerPresent(GetCurrentProcess(), &mut remote).is_ok()
            && remote.as_bool()
        {
            return true;
        }
        false
    }
}

/// Stall before doing anything interesting so short-lived automated sandboxes
/// time out first. Keep it modest; long sleeps also hurt real engagements.
pub fn sandbox_delay() {
    std::thread::sleep(std::time::Duration::from_secs(8));
}
