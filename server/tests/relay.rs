//! End-to-end relay test: boot the real server binary, connect a fake
//! implant + a fake operator, verify text control frames relay both ways.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn relay_both_directions() {
    let port: u16 = 8137;
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

    let id = "test-session-1";
    let (mut imp, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/implant"))
            .await
            .expect("implant connect");
    let hello = serde_json::json!({
        "t":"hello","id":id,"host":"HOST","user":"user","os":"Windows 11",
        "arch":"x64","pid":1,"admin":false,"screen":[800,600]
    });
    imp.send(Message::Text(hello.to_string())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    let (mut op, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/op/{id}"))
            .await
            .expect("op connect");
    tokio::time::sleep(Duration::from_millis(250)).await;

    // implant -> operator
    imp.send(Message::Text(
        serde_json::json!({"t":"cmd_result","id":"x","out":"hello-from-implant"}).to_string(),
    ))
    .await
    .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), op.next())
        .await
        .expect("timeout waiting implant->op")
        .expect("op closed")
        .unwrap();
    match got {
        Message::Text(t) => assert!(t.contains("hello-from-implant"), "got: {t}"),
        other => panic!("expected text, got {other:?}"),
    }

    // operator -> implant
    op.send(Message::Text(
        serde_json::json!({"t":"ping","id":7}).to_string(),
    ))
    .await
    .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), imp.next())
        .await
        .expect("timeout waiting op->implant")
        .expect("implant closed")
        .unwrap();
    match got {
        Message::Text(t) => assert!(t.contains("\"ping\""), "got: {t}"),
        other => panic!("expected text, got {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
}
