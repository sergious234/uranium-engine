use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::http::{HeaderName, HeaderValue, Method, header};
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
        active_operations: Mutex::new(HashSet::new()),
        auth_token: std::env::var("URANIUM_API_TOKEN")
            .expect("URANIUM_API_TOKEN must be set before starting the engine"),
    });

    tokio::spawn(routes::instances::resume_pending(state.clone()));

    let app = routes::router(state).layer(
        CorsLayer::new()
            .allow_origin([
                HeaderValue::from_static("tauri://localhost"),
                HeaderValue::from_static("http://tauri.localhost"),
                HeaderValue::from_static("http://localhost:1420"),
                HeaderValue::from_static("http://127.0.0.1:1420"),
            ])
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PATCH,
                Method::PUT,
                Method::DELETE,
            ])
            .allow_headers([
                header::CONTENT_TYPE,
                HeaderName::from_static("x-uranium-token"),
            ])
            .expose_headers([HeaderName::from_static("x-uranium-engine")]),
    );

    let addr = "127.0.0.1:13715";
    tracing::info!("Starting uranium-engine on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind address");

    axum::serve(listener, app).await.expect("Server failed");
}
