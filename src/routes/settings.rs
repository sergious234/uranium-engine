use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::AppError;
use crate::paths;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/settings", get(get_settings).put(put_settings))
}

/// Application settings persisted to `~/.config/uranium-engine/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Settings {
    /// Path to a custom Java executable. If `None`, the Mojang bundled runtime
    /// is used (downloaded via `RuntimeDownloader`).
    pub java_path: Option<String>,
    /// Max memory for the JVM, e.g. `"2G"` or `"4096M"`. Default: `"2G"`.
    pub max_memory: Option<String>,
    /// Additional JVM arguments to pass when launching Minecraft.
    pub jvm_args: Option<Vec<String>>,
    /// Initial window width in pixels. Default: `854`.
    pub window_width: Option<u32>,
    /// Initial window height in pixels. Default: `480`.
    pub window_height: Option<u32>,
    /// Whether to show the Minecraft launcher window. Default: `false`.
    pub show_launcher: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            java_path: None,
            max_memory: Some("2G".into()),
            jvm_args: None,
            window_width: Some(854),
            window_height: Some(480),
            show_launcher: Some(false),
        }
    }
}

/// `GET /settings` — read the current settings.
///
/// If `config.toml` does not exist, returns sensible defaults.
#[utoipa::path(
    get,
    path = "/settings",
    tag = "settings",
    responses(
        (status = 200, description = "Current settings", body = Settings),
    )
)]
async fn get_settings() -> Result<Json<Settings>, AppError> {
    Ok(Json(load_settings()?))
}

/// Load the same settings used by the API and the launch pipeline.
pub fn load_settings() -> Result<Settings, AppError> {
    let config_path = paths::config_file();
    let settings = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content).unwrap_or_default()
    } else {
        Settings::default()
    };
    Ok(settings)
}

/// `PUT /settings` — write new settings.
///
/// Accepts a partial or complete [`Settings`] JSON body. Missing fields are
/// set to `null` (which the server interprets as "use default").
#[utoipa::path(
    put,
    path = "/settings",
    tag = "settings",
    request_body = Settings,
    responses(
        (status = 200, description = "Settings saved", body = Settings),
    )
)]
async fn put_settings(Json(settings): Json<Settings>) -> Result<Json<Settings>, AppError> {
    let config_path = paths::config_file();
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(&settings)
        .map_err(|e| AppError::Internal(format!("Failed to serialize settings: {e}")))?;
    std::fs::write(&config_path, content)?;
    Ok(Json(settings))
}
