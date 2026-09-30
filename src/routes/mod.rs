use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use std::sync::Arc;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::state::AppState;

pub mod instances;
pub mod launcher;
pub mod modpacks;
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
        modpacks::create_mrpack_instance,
        modpacks::install_instance_loader,
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
            crate::db::instances::ModpackSource,
            instances::CreateInstanceRequest,
            instances::CreateInstanceResponse,
            instances::PatchInstanceRequest,
            modpacks::CreateMrpackInstanceRequest,
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
        .merge(modpacks::router())
        .merge(launcher::router())
        .merge(settings::router())
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state)
}

async fn require_token(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let supplied = if request.uri().path() == "/ws" {
        request.uri().query().and_then(|query| {
            query
                .split('&')
                .find_map(|part| part.strip_prefix("token="))
        })
    } else {
        request
            .headers()
            .get(HeaderName::from_static("x-uranium-token"))
            .and_then(|value| value.to_str().ok())
    };
    if supplied != Some(state.auth_token.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-uranium-engine"),
        HeaderValue::from_static("0.1.0"),
    );
    Ok(response)
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
