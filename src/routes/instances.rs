use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use uranium_rs::downloaders::{Downloader, MinecraftDownloadState, MinecraftDownloader};

use crate::db;
use crate::error::AppError;
use crate::events::AppEvent;
use crate::paths;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/instances", get(list_instances).post(create_instance))
        .route(
            "/instances/{id}",
            get(get_instance)
                .patch(patch_instance)
                .delete(delete_instance),
        )
}

/// Request body for [`create_instance`].
#[derive(Debug, Deserialize)]
pub struct CreateInstanceRequest {
    /// Display name for the new instance (e.g. `"My 1.21"`).
    name: String,
    /// Minecraft version string (e.g. `"1.21"`, `"1.20.4"`).
    version: String,
    /// Optional icon name. Defaults to `"Grass"` if omitted.
    /// Matches Minecraft launcher profile icon names.
    icon: Option<String>,
}

/// Response returned by [`create_instance`] on success (202 Accepted).
#[derive(Debug, Serialize)]
pub struct CreateInstanceResponse {
    /// The UUID assigned to the new instance.
    instance_id: String,
}

/// Request body for [`patch_instance`].
#[derive(Debug, Deserialize)]
pub struct PatchInstanceRequest {
    /// New name for the instance. Omit to keep existing.
    name: Option<String>,
    /// New icon for the instance. Omit to keep existing.
    icon: Option<String>,
    /// New runtime path. Omit to keep existing.
    java_runtime: Option<String>,
    /// New java args. Omit to keep existing.
    java_args: Option<String>
}

/// `GET /instances` — list all instances.
///
/// Returns an array of [`Instance`](crate::db::instances::Instance) objects
/// ordered by creation date (newest first).
async fn list_instances(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<db::instances::Instance>>, AppError> {
    let db = state.db.lock().unwrap();
    let instances = db::instances::get_all(&db)?;
    Ok(Json(instances))
}

/// `GET /instances/{id}` — get a single instance by ID.
///
/// Returns 404 if the instance does not exist.
async fn get_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<db::instances::Instance>, AppError> {
    let db = state.db.lock().unwrap();
    match db::instances::get(&db, &id)? {
        Some(instance) => Ok(Json(instance)),
        None => Err(AppError::NotFound(format!("Instance {id} not found"))),
    }
}

/// `POST /instances` — create a new Minecraft instance.
///
/// The instance is created with status `"downloading"` and a background task
/// immediately begins downloading the requested Minecraft version. WebSocket
/// clients will receive [`instance:progress`](AppEvent::InstanceProgress)
/// events during the download and either
/// [`instance:completed`](AppEvent::InstanceCompleted) or
/// [`instance:error`](AppEvent::InstanceError) when finished.
///
/// Returns **202 Accepted** with the new instance UUID.
async fn create_instance(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateInstanceRequest>,
) -> Result<(StatusCode, Json<CreateInstanceResponse>), AppError> {
    let instance_id = Uuid::new_v4().to_string();
    let game_dir = paths::instances_dir().join(&instance_id);
    let now = chrono::Utc::now().to_rfc3339();

    let instance = db::instances::Instance {
        id: instance_id.clone(),
        name: req.name.clone(),
        game_version: req.version.clone(),
        icon: req.icon.unwrap_or_else(|| "Grass".to_string()),
        game_dir: game_dir.to_string_lossy().to_string(),
        status: "downloading".to_string(),
        created_at: now,
        last_played: None,
        playtime_seconds: 0,
        java_runtime: "java".to_string(),
        java_args: "".to_string()
    };

    {
        let db = state.db.lock().unwrap();
        db::instances::insert(&db, &instance)?;
    }

    let state_clone = state.clone();
    let instance_clone = instance.clone();
    tokio::spawn(async move {
        background_download(state_clone, instance_clone, game_dir).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateInstanceResponse { instance_id }),
    ))
}

/// Background task that downloads Minecraft files for a newly created instance.
///
/// The task loops through [`MinecraftDownloader::progress`], broadcasting
/// [`AppEvent::InstanceProgress`] after each step. On completion it writes
/// `launcher_profiles.json`, calls `add_instance()` in the profiles file,
/// updates the DB status to `"ready"`, and broadcasts
/// [`AppEvent::InstanceCompleted`]. On error it sets status to `"error"` and
/// broadcasts [`AppEvent::InstanceError`].
async fn background_download(
    state: Arc<AppState>,
    instance: db::instances::Instance,
    game_dir: std::path::PathBuf,
) {
    let instance_id = instance.id.clone();
    let name = instance.name.clone();
    let icon = instance.icon.clone();
    let version = instance.game_version.clone();

    if let Err(e) = std::fs::create_dir_all(&game_dir) {
        tracing::error!("Failed to create game dir: {e}");
        let _ = state.event_tx.send(AppEvent::InstanceError {
            instance_id: instance_id.clone(),
            error: e.to_string(),
        });
        let db = state.db.lock().unwrap();
        let _ = db::instances::update_status(&db, &instance_id, "error");
        return;
    }

    let mut downloader = match MinecraftDownloader::<Downloader>::init(&game_dir, &version).await {
        Ok(d) => d,
        Err(e) => {
            let err_msg = e.to_string();
            tracing::error!("Download init failed: {err_msg}");
            let _ = state.event_tx.send(AppEvent::InstanceError {
                instance_id: instance_id.clone(),
                error: err_msg,
            });
            let db = state.db.lock().unwrap();
            let _ = db::instances::update_status(&db, &instance_id, "error");
            return;
        }
    };

    loop {
        match downloader.progress().await {
            Ok(MinecraftDownloadState::Completed) => break,
            Ok(ds) => {
                let phase = format!("{ds:?}");
                let remaining = downloader.requests_left();
                let _ = state.event_tx.send(AppEvent::InstanceProgress {
                    instance_id: instance_id.clone(),
                    phase,
                    remaining,
                });
            }
            Err(e) => {
                let err_msg = e.to_string();
                tracing::error!("Download failed: {err_msg}");
                let _ = state.event_tx.send(AppEvent::InstanceError {
                    instance_id: instance_id.clone(),
                    error: err_msg,
                });
                let db = state.db.lock().unwrap();
                let _ = db::instances::update_status(&db, &instance_id, "error");
                return;
            }
        }
    }

    let profiles_path = game_dir.join("launcher_profiles.json");
    if !profiles_path.exists() {
        let profiles = uranium_rs::mine_data_structs::minecraft::ProfilesJson::default();
        if let Ok(content) = serde_json::to_string_pretty(&profiles) {
            let _ = std::fs::write(&profiles_path, content);
        }
    }

    if let Err(e) = downloader.add_instance(&game_dir, &name, Some(&icon)) {
        tracing::warn!("add_instance failed: {e}");
    }

    {
        let db = state.db.lock().unwrap();
        let _ = db::instances::update_status(&db, &instance_id, "ready");
    }
    let _ = state.event_tx.send(AppEvent::InstanceCompleted {
        instance_id: instance_id.clone(),
    });
}

/// `PATCH /instances/{id}` — update an instance's name and/or icon.
///
/// Both fields are optional; only provided fields are updated.
/// Returns the updated [`Instance`](crate::db::instances::Instance).
async fn patch_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<PatchInstanceRequest>,
) -> Result<Json<db::instances::Instance>, AppError> {
    let db = state.db.lock().unwrap();
    let mut instance = db::instances::get(&db, &id)?
        .ok_or_else(|| AppError::NotFound(format!("Instance {id} not found")))?;

    if let Some(name) = &req.name {
        db::instances::rename(&db, &id, name)?;
        instance.name = name.clone();
    }
    if let Some(icon) = &req.icon {
        db::instances::update_icon(&db, &id, icon)?;
        instance.icon = icon.clone();
    }

    if let Some(runtime) = &req.java_runtime {
        db::instances::update_runtime(&db, &id, runtime)?;
        instance.java_runtime = runtime.clone();
    }

    if let Some(args) = &req.java_args {
        db::instances::update_args(&db, &id, args)?;
        instance.java_args = args.clone();
    }

    Ok(Json(instance))
}

/// `DELETE /instances/{id}` — delete an instance.
///
/// Removes the instance from the database and recursively deletes its game
/// directory. Returns 404 if the instance does not exist.
async fn delete_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let db = state.db.lock().unwrap();
    let instance = db::instances::get(&db, &id)?
        .ok_or_else(|| AppError::NotFound(format!("Instance {id} not found")))?;

    db::instances::delete(&db, &id)?;

    let game_dir = std::path::Path::new(&instance.game_dir);
    if game_dir.exists() {
        let _ = std::fs::remove_dir_all(game_dir);
    }

    Ok(Json(serde_json::json!({ "deleted": id })))
}
