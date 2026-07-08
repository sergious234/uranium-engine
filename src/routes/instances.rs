use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use fs_extra::dir::get_size;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use tracing::{info, warn};

use uranium_rs::downloaders::list_instances as get_mc_versions;
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
        .route("/mc-versions", get(mc_versions))
        .route("/instances/clean", get(clean))
}

/// Request body for [`create_instance`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInstanceRequest {
    /// Display name for the new instance (e.g. `"My 1.21"`).
    #[schema(example = "My 1.21 Survival")]
    name: String,
    /// Minecraft version string (e.g. `"1.21"`, `"1.20.4"`).
    #[schema(example = "1.21")]
    version: String,
    /// Optional icon name. Defaults to `"Grass"` if omitted.
    /// Matches Minecraft launcher profile icon names.
    #[schema(example = "Diamond")]
    icon: Option<String>,
    #[schema(example = "-Xmx1024G")]
    java_args: Option<String>,
}

/// Response returned by [`create_instance`] on success (202 Accepted).
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateInstanceResponse {
    /// The UUID assigned to the new instance.
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    instance_id: String,
}

/// Request body for [`patch_instance`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchInstanceRequest {
    /// New name for the instance. Omit to keep existing.
    #[schema(example = "Renamed Instance")]
    name: Option<String>,
    /// New icon for the instance. Omit to keep existing.
    #[schema(example = "Grass")]
    icon: Option<String>,
    /// New runtime path. Omit to keep existing.
    #[schema(example = "/usr/lib/jvm/java-21-openjdk/bin/java")]
    java_runtime: Option<String>,
    /// New java args. Omit to keep existing.
    #[schema(example = "-Xmx4G -XX:+UseG1GC")]
    java_args: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CleanResponse {
    pub removed_instances: Vec<String>,
    pub freed_memory: u64,
}

/// `GET /mc-versions` — list of all minecraft versions.
///
/// Returns an array of `String`.
#[utoipa::path(
    get,
    path = "/mc-versions",
    tag = "mc",
    responses(
        (status = 200, description = "Array of all versions"),
    )
)]
async fn mc_versions(State(_state): State<Arc<AppState>>) -> Result<Json<Vec<String>>, AppError> {
    let mc_versions = get_mc_versions()
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?;
    let ids = Vec::from_iter(mc_versions.versions.into_iter().map(|v| v.id));
    Ok(Json(ids))
}

/// `GET /instances` — list all instances.
///
/// Returns an array of [`Instance`](crate::db::instances::Instance) objects
/// ordered by creation date (newest first).
#[utoipa::path(
    get,
    path = "/instances",
    tag = "instances",
    responses(
        (status = 200, description = "Array of all instances", body = Vec<crate::db::instances::Instance>),
    )
)]
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
#[utoipa::path(
    get,
    path = "/instances/{id}",
    tag = "instances",
    params(
        ("id" = String, Path, description = "Instance UUID"),
    ),
    responses(
        (status = 200, description = "Instance found", body = crate::db::instances::Instance),
        (status = 404, description = "Instance not found"),
    )
)]
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
#[utoipa::path(
    post,
    path = "/instances",
    tag = "instances",
    request_body = CreateInstanceRequest,
    responses(
        (status = 202, description = "Instance created, download started", body = CreateInstanceResponse),
        (status = 500, description = "Internal server error"),
    )
)]
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
        status: db::instances::InstanceStatus::Downloading,
        created_at: now,
        last_played: None,
        playtime_seconds: 0,
        java_runtime: "java".to_string(),
        java_args: req.java_args.unwrap_or_default(),
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
                info!("Remaining requests: {remaining}");
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

/// `PATCH /instances/{id}` — update an instance's name, icon, runtime, and/or args.
///
/// All fields are optional; only provided fields are updated.
/// Returns the updated [`Instance`](crate::db::instances::Instance).
#[utoipa::path(
    patch,
    path = "/instances/{id}",
    tag = "instances",
    params(
        ("id" = String, Path, description = "Instance UUID"),
    ),
    request_body = PatchInstanceRequest,
    responses(
        (status = 200, description = "Instance updated", body = crate::db::instances::Instance),
        (status = 404, description = "Instance not found"),
    )
)]
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
#[utoipa::path(
    delete,
    path = "/instances/{id}",
    tag = "instances",
    params(
        ("id" = String, Path, description = "Instance UUID"),
    ),
    responses(
        (status = 200, description = "Instance deleted successfully"),
        (status = 404, description = "Instance not found"),
    )
)]
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

/// `POST /instances/clean` — remove orphaned instance directories.
///
/// Scans the instances data directory and removes any subdirectory that does
/// not correspond to an instance in the database. Useful for cleaning up after
/// manual filesystem changes or failed instance deletions.
///
/// Returns **200 OK** with a [`CleanResponse`] containing the list of removed
/// directory names and the total freed disk space in bytes.
#[utoipa::path(
    get,
    path = "/instances/clean",
    tag = "instances",
    responses(
        (status = 200, description = "Orphaned directories removed", body = CleanResponse),
    )
)]
async fn clean(State(state): State<Arc<AppState>>) -> Result<Json<CleanResponse>, AppError> {
    use crate::db::instances::get_all;
    let instances = get_all(&state.db.lock().unwrap())?;
    let instances_dir = paths::instances_dir();

    let mut freed_memory = 0;
    let mut removed_instances = vec![];
    for entry in instances_dir.read_dir()? {
        info!("Entry {:?}", entry.as_ref().unwrap().path());
        if let Ok(dir) = entry
            && !instances
                .iter()
                .any(|i| std::path::Path::new(&i.game_dir) == dir.path())
        {
            match std::fs::remove_dir_all(dir.path()) {
                Ok(_) => {
                    info!(
                        "{:?}\n{:?} is not in the DataBase, will be removed.",
                        dir.path(),
                        dir.file_name()
                    );
                    freed_memory += get_size(dir.path()).unwrap_or(2);
                    removed_instances.push(dir.file_name().display().to_string());
                }
                Err(err) => warn!("Error when removing {:?}, {err}", dir.file_name()),
            };
        }
    }

    Ok(Json(CleanResponse{removed_instances, freed_memory}))
}
