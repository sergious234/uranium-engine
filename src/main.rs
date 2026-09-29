use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;

use uranium_engine::db;
use uranium_engine::events;
use uranium_engine::paths;
use uranium_engine::routes;
use uranium_engine::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(e) = paths::ensure_dirs() {
        tracing::error!("Failed to create directories: {e}");
        std::process::exit(1);
    }

    let db_path = paths::db_file();
    let conn = match db::init(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::error!("Failed to initialize database: {e}");
            std::process::exit(1);
        }
    };

    let (event_tx, _) = events::new_event_channel();

    let state = Arc::new(AppState {
        db: Mutex::new(conn),
        event_tx,
        running: Arc::new(Mutex::new(HashMap::new())),
    });

    let app = routes::router(state).layer(CorsLayer::permissive());

    let addr = "127.0.0.1:13715";
    tracing::info!("Starting uranium-engine on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind address");

    axum::serve(listener, app).await.expect("Server failed");
}
