use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

pub mod instances;
pub mod launcher;
pub mod settings;
pub mod ws;

/// Build the merged application router with all endpoints.
///
/// All sub-routers are merged and [`AppState`] is injected via `.with_state()`.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", axum::routing::get(health))
        .merge(ws::router())
        .merge(instances::router())
        .merge(launcher::router())
        .merge(settings::router())
        .with_state(state)
}

/// `GET /health` — server health check.
///
/// Returns `200 OK` with body `"OK"`. Used by clients to verify the server
/// is running and reachable.
async fn health() -> &'static str {
    "OK"
}
