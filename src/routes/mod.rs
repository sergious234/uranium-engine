use axum::Router;
use std::sync::Arc;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::state::AppState;

pub mod instances;
pub mod launcher;
pub mod settings;
pub mod ws;

#[derive(OpenApi)]
#[openapi(
    paths(
        health,
        ws::ws_handler,
        instances::list_instances,
        instances::create_instance,
        instances::get_instance,
        instances::patch_instance,
        instances::delete_instance,
        launcher::launch_instance,
        launcher::terminate_instance,
        launcher::list_running,
        settings::get_settings,
        settings::put_settings,
        instances::mc_versions,
        instances::clean
    ),
    components(
        schemas(
            crate::events::AppEvent,
            crate::db::instances::Instance,
            instances::CreateInstanceRequest,
            instances::CreateInstanceResponse,
            instances::PatchInstanceRequest,
            launcher::LaunchResponse,
            launcher::RunningResponse,
            launcher::RunningEntry,
            settings::Settings,
        )
    ),
    tags(
        (name = "events", description = "Real-time event stream via WebSocket — broadcasts instance and game lifecycle events as JSON text frames"),
        (name = "health", description = "Server health checks"),
        (name = "instances", description = "Minecraft instance CRUD — create, list, get, patch, and delete instances"),
        (name = "launcher", description = "Launch, terminate, and query running Minecraft game instances"),
        (name = "settings", description = "Application settings — read and write persistent configuration"),
        (name = "mc", description = "Minecraft related information or data"),
    )
)]
pub struct ApiDoc;

/// Build the merged application router with all endpoints.
///
/// All sub-routers are merged and [`AppState`] is injected via `.with_state()`.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", axum::routing::get(health))
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", ApiDoc::openapi()))
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
#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses(
        (status = 200, description = "Server is healthy and running", body = String, example = json!("OK")),
    )
)]
async fn health() -> &'static str {
    "OK"
}
