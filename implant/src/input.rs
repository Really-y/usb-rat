//! Remote input: cursor placement + mouse buttons via mouse_event,
//! keyboard via SendInput with raw Windows virtual-key codes.
#![cfg(windows)]

use windows::Win32::UI::Input::KeyboardAndMouse::{
    mouse_event, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, KEYBD_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

pub fn mouse(x: f32, y: f32, btn: &str, act: &str, sw: i32, sh: i32) {
    if sw <= 0 || sh <= 0 {
        return;
    }
    let px = (x.clamp(0.0, 1.0) * sw as f32) as i32;
    let py = (y.clamp(0.0, 1.0) * sh as f32) as i32;
    unsafe {
        match act {
            "move" => {
                let _ = SetCursorPos(px, py);
            }
            "down" | "up" => {
                let _ = SetCursorPos(px, py);
                let flags: MOUSE_EVENT_FLAGS = match (btn, act) {
                    ("right", "down") => MOUSEEVENTF_RIGHTDOWN,
                    ("right", "up") => MOUSEEVENTF_RIGHTUP,
                    ("middle", "down") => MOUSEEVENTF_MIDDLEDOWN,
                    ("middle", "up") => MOUSEEVENTF_MIDDLEUP,
                    (_, "down") => MOUSEEVENTF_LEFTDOWN,
                    (_, "up") => MOUSEEVENTF_LEFTUP,
                    _ => return,
                };
                mouse_event(flags, 0, 0, 0, 0);
            }
            _ => {}
        }
    }
}

/// Type a string as Unicode input, independent of the keyboard layout.
pub fn text(s: &str) {
    for u in s.encode_utf16() {
        unsafe {
            let mut down: INPUT = std::mem::zeroed();
            down.r#type = INPUT_KEYBOARD;
            down.Anonymous.ki = KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: u,
                dwFlags: KEYEVENTF_UNICODE,
                time: 0,
                dwExtraInfo: 0,
            };
            SendInput(&[down], std::mem::size_of::<INPUT>() as i32);

            let mut up: INPUT = std::mem::zeroed();
            up.r#type = INPUT_KEYBOARD;
            up.Anonymous.ki = KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: u,
                dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            };
            SendInput(&[up], std::mem::size_of::<INPUT>() as i32);
        }
    }
}

pub fn key(vk: u16, down: bool) {
    if vk == 0 {
        return;
    }
    unsafe {
        let mut inp: INPUT = std::mem::zeroed();
        inp.r#type = INPUT_KEYBOARD;
        inp.Anonymous.ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(vk),
            wScan: 0,
            dwFlags: if down {
                KEYBD_EVENT_FLAGS(0)
            } else {
                KEYEVENTF_KEYUP
            },
            time: 0,
            dwExtraInfo: 0,
        };
        SendInput(&[inp], std::mem::size_of::<INPUT>() as i32);
    }
}
