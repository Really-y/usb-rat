//! Embed a folder icon into the loader's PE resources so the delivered
//! `Sənədlər.exe` looks like a folder (no shortcut arrow overlay).

use std::path::PathBuf;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest"));
    let rc = manifest.join("icon.rc");
    let ico = manifest.join("folder.ico");
    println!("cargo:rerun-if-changed={}", rc.display());
    println!("cargo:rerun-if-changed={}", ico.display());

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("icon.res.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let status = std::process::Command::new(&windres)
        .args([
            "-I",
            manifest.to_str().expect("utf8 manifest"),
            "-i",
            rc.to_str().expect("utf8 rc"),
            "-o",
            out.to_str().expect("utf8 out"),
        ])
        .status()
        .expect("spawn windres");
    assert!(status.success(), "windres failed");
    println!("cargo:rustc-link-arg={}", out.display());
    // Keep the resource section from being garbage-collected by the linker.
    println!("cargo:rustc-link-arg=-Wl,--no-gc-sections");
}
