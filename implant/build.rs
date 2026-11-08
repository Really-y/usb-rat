//! Bridge a local `.env` into rustc env vars so the compile-time
//! `option_env!` configuration in the implant sees it.
//!
//! Cargo does not read `.env` files. This script reads the workspace-root
//! `.env` (or `implant/.env`) and emits `cargo:rustc-env=KEY=VALUE` for every
//! assignment. A real process environment variable always wins, so CI can
//! override the local file. `.env` is gitignored; the token is baked into the
//! built binary, never into the source tree.

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let candidates = [
        Path::new(&manifest).join("../.env"),
        Path::new(&manifest).join(".env"),
    ];
    for p in &candidates {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let Some(file) = candidates.iter().find(|p| p.is_file()) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if key.is_empty() || value.is_empty() {
            continue;
        }
        if std::env::var_os(key).is_none() {
            println!("cargo:rustc-env={key}={value}");
        }
    }
}
