//! Synthetic implant for a live end-to-end demo (NOT the Windows agent).
//! Speaks the real protocol: streams an animated desktop, moves a cursor dot
//! to wherever the operator clicks/drags, and answers `cmd`/`ping`.
//!
//!   C2_URL=ws://127.0.0.1:8080/ws/implant cargo run -p c2-server --example fake_implant

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;

const W: u32 = 960;
const H: u32 = 540;

#[derive(Clone, Copy)]
struct Cfg {
    fps: u32,
    quality: u8,
    scale: f32,
}

struct State {
    cfg: Cfg,
    cur: (f32, f32),
    tick: u32,
}

#[tokio::main]
async fn main() {
    let url = std::env::var("C2_URL").unwrap_or_else(|_| "ws://127.0.0.1:8080/ws/implant".into());
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .unwrap_or_else(|e| panic!("connect {url} failed: {e}"));
    let (mut write, mut read) = ws.split();

    let (tx, mut rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if write.send(m).await.is_err() {
                break;
            }
        }
    });

    let st = Arc::new(RwLock::new(State {
        cfg: Cfg {
            fps: 10,
            quality: 55,
            scale: 0.75,
        },
        cur: (0.5, 0.5),
        tick: 0,
    }));

    let hello = json!({
        "t":"hello","id":"demo-0001","host":"DEMO-WS01","user":"operator",
        "os":"Windows 11 (simulated)","arch":"x64","pid":4242,"admin":false,"screen":[W,H]
    });
    tx.send(Message::Text(hello.to_string())).await.unwrap();
    println!("[demo] connected to {url} as DEMO-WS01");

    let pump = {
        let tx = tx.clone();
        let st = st.clone();
        tokio::spawn(async move { frame_loop(tx, st).await })
    };

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
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
                        let _ = tx
                            .send(Message::Text(json!({"t":"pong","id":id}).to_string()))
                            .await;
                    }
                    "mouse" => {
                        let mut s = st.write().await;
                        s.cur.0 = v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.5) as f32;
                        s.cur.1 = v.get("y").and_then(|x| x.as_f64()).unwrap_or(0.5) as f32;
                    }
                    "cmd" => {
                        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let line = v.get("line").and_then(|x| x.as_str()).unwrap_or("");
                        println!("[demo] cmd: {line}");
                        let out = format!(
                            "(simulated) executed: {line}\r\nOS: Windows 11 sim\r\nuser: operator\r\nhost: DEMO-WS01\r\n"
                        );
                        let _ = tx
                            .send(Message::Text(
                                json!({"t":"cmd_result","id":id,"out":out}).to_string(),
                            ))
                            .await;
                    }
                    "set" => {
                        let mut s = st.write().await;
                        if let Some(x) = v.get("fps").and_then(|x| x.as_u64()) {
                            s.cfg.fps = (x as u32).clamp(1, 15);
                        }
                        if let Some(x) = v.get("quality").and_then(|x| x.as_u64()) {
                            s.cfg.quality = (x as u8).clamp(20, 80);
                        }
                        if let Some(x) = v.get("scale").and_then(|x| x.as_f64()) {
                            s.cfg.scale = (x as f32).clamp(0.3, 1.0);
                        }
                    }
                    _ => {}
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    pump.abort();
    writer.abort();
    println!("[demo] disconnected");
}

async fn frame_loop(tx: mpsc::Sender<Message>, st: Arc<RwLock<State>>) {
    let base = base_image();
    loop {
        let (cfg, cur, tick) = {
            let mut s = st.write().await;
            s.tick = s.tick.wrapping_add(1);
            (s.cfg, s.cur, s.tick)
        };
        let period = Duration::from_millis(1000 / cfg.fps.max(1) as u64);

        let mut img = base.clone();
        draw_sweep(&mut img, tick);
        draw_cursor(&mut img, cur);

        if let Some(jpeg) = encode(&img, cfg.scale, cfg.quality) {
            let mut frame = Vec::with_capacity(jpeg.len() + 1);
            frame.push(0x01);
            frame.extend_from_slice(&jpeg);
            if tx.send(Message::Binary(frame)).await.is_err() {
                break;
            }
        }
        tokio::time::sleep(period).await;
    }
}

fn base_image() -> image::RgbImage {
    let mut img = image::RgbImage::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let grid = if x % 80 == 0 || y % 80 == 0 { 10u8 } else { 0 };
            let v = ((x * 255 / W) as u8 / 7) + ((y * 255 / H) as u8 / 9);
            let r = 12u8.saturating_add(v / 2).saturating_add(grid);
            let g = 14u8.saturating_add(v / 2).saturating_add(grid);
            let b = 20u8.saturating_add(v).saturating_add(grid);
            img.put_pixel(x, y, image::Rgb([r, g, b]));
        }
    }
    img
}

fn draw_sweep(img: &mut image::RgbImage, tick: u32) {
    let x0 = (tick.wrapping_mul(11)) % W;
    for x in x0..(x0 + 48).min(W) {
        for y in 0..H {
            let p = img.get_pixel_mut(x, y);
            p.0[0] = p.0[0].saturating_add(30);
            p.0[1] = p.0[1].saturating_add(42);
            p.0[2] = p.0[2].saturating_add(24);
        }
    }
}

fn draw_cursor(img: &mut image::RgbImage, cur: (f32, f32)) {
    let cx = (cur.0.clamp(0.0, 1.0) as f32 * (W as f32 - 1.0)) as i32;
    let cy = (cur.1.clamp(0.0, 1.0) as f32 * (H as f32 - 1.0)) as i32;
    for dy in -14..=14 {
        for dx in -14..=14 {
            if dx * dx + dy * dy <= 14 * 14 {
                let x = (cx + dx).clamp(0, W as i32 - 1) as u32;
                let y = (cy + dy).clamp(0, H as i32 - 1) as u32;
                img.put_pixel(x, y, image::Rgb([255, 186, 52]));
            }
        }
    }
}

fn encode(img: &image::RgbImage, scale: f32, quality: u8) -> Option<Vec<u8>> {
    let scale = scale.clamp(0.3, 1.0);
    let (nw, nh) = (
        ((W as f32 * scale) as u32).max(1),
        ((H as f32 * scale) as u32).max(1),
    );
    let scaled = if nw == W && nh == H {
        img.clone()
    } else {
        image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle)
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(scaled.as_raw(), nw, nh, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(buf.into_inner())
}
