use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use uranium_rs::engine::{DownloadState, Downloader, FileDownloader};
use uranium_rs::error::UraniumError;
use uranium_rs::modpacks::rinth::{
    FabricStep, LoaderCtx, LoaderInstallStep, LoaderKind, QuiltStep, RinthInstallState,
    RinthInstaller, Side,
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::db;
use crate::error::AppError;
use crate::events::{AppEvent, PhaseProgressTracker};
use crate::paths;
use crate::routes::instances::{CreateInstanceResponse, finalize_download};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/instances/mrpack", post(create_mrpack_instance))
        .route("/instances/{id}/loader", post(install_instance_loader))
}

/// Request body for [`create_mrpack_instance`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMrpackInstanceRequest {
    /// Display name for the new instance (e.g. `"Fabulously Optimized"`).
    #[schema(example = "Fabulously Optimized")]
    name: String,
    /// Local server-side path to the `.mrpack` file.
    #[schema(example = "/home/user/Downloads/pack.mrpack")]
    mrpack_path: String,
    /// Optional icon name. Defaults to `"Grass"` if omitted.
    #[schema(example = "Diamond")]
    icon: Option<String>,
    /// Optional extra JVM arguments for the instance.
    #[schema(example = "-Xmx4G")]
    java_args: Option<String>,
    /// Override the Minecraft version instead of using the pack's
    /// `dependencies.minecraft`. Required when the pack declares none.
    #[schema(example = "1.21")]
    version_override: Option<String>,
}

/// `POST /instances/mrpack` — create a new instance from a Modrinth pack.
///
/// Validates the local `.mrpack` path, parses its manifest, and inserts the
/// instance with status `"downloading"`. A background task then runs the full
/// [`RinthInstaller`] pipeline (client side): vanilla Minecraft for the
/// pack's `dependencies.minecraft`, the declared loader (Fabric/Quilt),
/// side-matching overrides, `env`-filtered mod files with SHA-1 verification.
///
/// WebSocket clients receive [`AppEvent::InstanceProgress`] with `phase` one
/// of `ResolvingManifest`, `InstallingMinecraft`, `InstallingLoader`,
/// `CopyingOverrides`, `DownloadingFiles`, `Verifying` (`InstallingLoader`
/// is skipped for vanilla packs), then either [`AppEvent::InstanceCompleted`]
/// or [`AppEvent::InstanceError`]. Phase strings are the
/// `RinthInstallState::as_str` values, forwarded verbatim. Each frame also
/// carries `remaining`/`total` batch counts per phase (same units), so
/// clients render `done = total - remaining`; `total` is absent while the
/// queue is still empty (first `InstallingMinecraft` poll).
///
/// Forge/NeoForge packs are rejected with 400: their installers are not
/// implemented yet. The installed loader profile id is stored on the
/// instance; the GUI gates Play on `loader_profile` being non-null.
///
/// Returns **202 Accepted** with the new instance UUID, **400** for a missing
/// path, a non-`.mrpack` file, an unreadable pack, an unresolvable game
/// version, or an unsupported loader.
#[utoipa::path(
    post,
    path = "/instances/mrpack",
    tag = "instances",
    request_body = CreateMrpackInstanceRequest,
    responses(
        (status = 202, description = "Modpack instance created, install started", body = CreateInstanceResponse),
        (status = 400, description = "Invalid mrpack path, unresolvable game version, or unsupported loader"),
        (status = 500, description = "Internal server error"),
    )
)]
async fn create_mrpack_instance(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateMrpackInstanceRequest>,
) -> Result<(StatusCode, Json<CreateInstanceResponse>), AppError> {
    // -- Validate the local pack path
    let mrpack_file = PathBuf::from(&req.mrpack_path);
    if !mrpack_file.is_file() {
        return Err(AppError::BadRequest(format!(
            "mrpack file not found: {}",
            req.mrpack_path
        )));
    }
    if mrpack_file.extension().and_then(|ext| ext.to_str()) != Some("mrpack") {
        return Err(AppError::BadRequest(format!(
            "not a .mrpack file: {}",
            req.mrpack_path
        )));
    }

    let instance_id = Uuid::new_v4().to_string();
    let game_dir = paths::instances_dir().join(&instance_id);
    std::fs::create_dir_all(&game_dir)?;

    // -- Parse the manifest off the async runtime (zip extraction + JSON).
    // -- Constructor failures are client errors (bad pack), not 500s.
    let task_dir = game_dir.clone();
    let task_pack = mrpack_file.clone();
    let installer = tokio::task::spawn_blocking(move || {
        RinthInstaller::<Downloader>::with_side(&task_pack, &task_dir, Side::Client)
    })
    .await
    .map_err(|e| AppError::Internal(format!("Installer task failed: {e}")))?
    .map_err(|e| match e {
        UraniumError::WrongFileFormat => {
            AppError::BadRequest(format!("not a valid .mrpack file: {}", req.mrpack_path))
        }
        UraniumError::FileNotFound(_) => {
            AppError::BadRequest(format!("mrpack file not found: {}", req.mrpack_path))
        }
        e => AppError::Uranium(e),
    })?;

    // -- Resolve the game version and loader before inserting the row.
    // -- Forge/NeoForge fail fast here: their install phase would error only
    // -- after a full vanilla download.
    let game_version = req
        .version_override
        .clone()
        .or_else(|| installer.pack_meta().minecraft_version.clone())
        .ok_or_else(|| {
            AppError::BadRequest(
                "pack declares no minecraft version; provide version_override".to_string(),
            )
        })?;
    let (loader, loader_version) = match installer.loader_requirement() {
        Some(req) => match req.loader {
            LoaderKind::Forge | LoaderKind::NeoForge => {
                return Err(AppError::BadRequest(format!(
                    "{:?} packs are not supported yet",
                    req.loader
                )));
            }
            _ => (Some(format!("{:?}", req.loader)), Some(req.version.clone())),
        },
        None => (None, None),
    };

    let instance = db::instances::Instance {
        id: instance_id.clone(),
        name: req.name.clone(),
        game_version,
        icon: req.icon.unwrap_or_else(|| "Grass".to_string()),
        game_dir: game_dir.to_string_lossy().to_string(),
        status: db::instances::InstanceStatus::Downloading,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_played: None,
        playtime_seconds: 0,
        java_runtime: "java".to_string(),
        java_args: req.java_args.unwrap_or_default(),
        modpack_source: db::instances::ModpackSource::Mrpack,
        modpack_path: Some(req.mrpack_path.clone()),
        loader,
        loader_version,
        loader_profile: None,
    };

    {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("DB lock poisoned: {e}")))?;
        db::instances::insert(&db, &instance)?;
    }

    let state_clone = state.clone();
    tokio::spawn(async move {
        background_mrpack_download(state_clone, instance, game_dir, installer).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateInstanceResponse { instance_id }),
    ))
}

/// Background task installing the pack files into a new instance.
///
/// Polls [`RinthInstaller::progress`], broadcasting
/// [`AppEvent::InstanceProgress`] per state. On completion it writes
/// `launcher_profiles.json`, registers the instance, updates the DB status
/// to `"ready"`, and broadcasts [`AppEvent::InstanceCompleted`]. On error it
/// sets status to `"error"` and broadcasts [`AppEvent::InstanceError`].
async fn background_mrpack_download(
    state: Arc<AppState>,
    instance: db::instances::Instance,
    game_dir: PathBuf,
    mut installer: RinthInstaller<Downloader>,
) {
    let instance_id = instance.id.clone();
    let mut tracker = PhaseProgressTracker::default();

    loop {
        match installer.progress().await {
            Ok(RinthInstallState::Completed) => break,
            Ok(install_state) => {
                let phase = install_state.as_str().to_string();
                let remaining = installer.requests_left();
                let total = tracker.update(&phase, remaining);
                let _ = state.event_tx.send(AppEvent::InstanceProgress {
                    instance_id: instance_id.clone(),
                    phase,
                    remaining,
                    total,
                });
            }
            Err(e) => {
                let err_msg = e.to_string();
                tracing::error!("Modpack install failed: {err_msg}");
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

    if let Some(profile) = installer.installed_profile_id() {
        let db = state.db.lock().unwrap();
        let _ = db::instances::update_loader_profile(&db, &instance_id, profile);
    }

    finalize_download(&game_dir, &instance);

    {
        let db = state.db.lock().unwrap();
        let _ = db::instances::update_status(&db, &instance_id, "ready");
    }
    let _ = state.event_tx.send(AppEvent::InstanceCompleted {
        instance_id: instance_id.clone(),
    });
}

/// Recreate an interrupted pack installer from the persisted source path.
/// If the user removed the original pack, record an error instead of leaving
/// the instance permanently in the downloading state.
pub async fn resume_mrpack(
    state: Arc<AppState>,
    instance: db::instances::Instance,
    game_dir: PathBuf,
) {
    let pack_path = match &instance.modpack_path {
        Some(path) => PathBuf::from(path),
        None => {
            fail_resumed_install(&state, &instance.id, "Pack source path is missing".into());
            return;
        }
    };
    let installer = tokio::task::spawn_blocking(move || {
        RinthInstaller::<Downloader>::with_side(&pack_path, &game_dir, Side::Client)
            .map(|installer| (installer, game_dir))
    })
    .await;
    match installer {
        Ok(Ok((installer, game_dir))) => {
            background_mrpack_download(state, instance, game_dir, installer).await;
        }
        Ok(Err(error)) => fail_resumed_install(&state, &instance.id, error.to_string()),
        Err(error) => fail_resumed_install(&state, &instance.id, error.to_string()),
    }
}

fn fail_resumed_install(state: &Arc<AppState>, instance_id: &str, error: String) {
    tracing::error!(instance_id, "Could not resume pack install: {error}");
    let db = state.db.lock().unwrap();
    if let Err(db_error) = db::instances::update_status(&db, instance_id, "error") {
        tracing::error!(instance_id, "Could not persist install failure: {db_error}");
    }
    let _ = state.event_tx.send(AppEvent::InstanceError {
        instance_id: instance_id.to_string(),
        error,
    });
}

/// `POST /instances/{id}/loader` — install (or re-install) the loader for a
/// modpack instance.
///
/// For instances stuck at `"ready"` with a declared loader but no installed
/// profile (`loader_profile` null) — including rows created before loader
/// support existed. Only Fabric/Quilt are supported.
///
/// Runs in the background: WebSocket clients receive
/// [`AppEvent::InstanceProgress`] with `phase: "InstallingLoader"`, then
/// either [`AppEvent::InstanceCompleted`] or [`AppEvent::InstanceError`].
/// A loader failure keeps status `"ready"` (the vanilla files are fine; the
/// instance is simply still pending).
///
/// Returns **202 Accepted**, **400** for a non-modpack instance, a missing
/// loader declaration, an unsupported or already-installed loader, or a
/// non-ready/running instance; **404** if the instance does not exist.
#[utoipa::path(
    post,
    path = "/instances/{id}/loader",
    tag = "instances",
    params(
        ("id" = String, Path, description = "Instance UUID"),
    ),
    responses(
        (status = 202, description = "Loader install started", body = CreateInstanceResponse),
        (status = 400, description = "Instance not eligible for loader install"),
        (status = 404, description = "Instance not found"),
    )
)]
async fn install_instance_loader(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<(StatusCode, Json<CreateInstanceResponse>), AppError> {
    let operation = state.reserve(&id)?;
    let instance = {
        let db = state
            .db
            .lock()
            .map_err(|e| AppError::Internal(format!("DB lock poisoned: {e}")))?;
        db::instances::get(&db, &id)?
            .ok_or_else(|| AppError::NotFound(format!("Instance {id} not found")))?
    };

    if instance.status != db::instances::InstanceStatus::Ready {
        return Err(AppError::BadRequest(format!(
            "Instance {id} is not ready (status: {})",
            instance.status
        )));
    }
    {
        let running = state.running.lock().unwrap();
        if running.contains_key(&id) {
            return Err(AppError::BadRequest(format!(
                "Instance {id} is already running"
            )));
        }
    }
    if instance.modpack_source != db::instances::ModpackSource::Mrpack {
        return Err(AppError::BadRequest(format!(
            "Instance {id} is not a modpack instance"
        )));
    }
    if instance.loader_profile.is_some() {
        return Err(AppError::BadRequest(format!(
            "Instance {id} loader already installed"
        )));
    }
    let loader_kind = match instance.loader.as_deref() {
        Some("Fabric") => LoaderKind::Fabric,
        Some("Quilt") => LoaderKind::Quilt,
        Some(other) => {
            return Err(AppError::BadRequest(format!(
                "{other} packs are not supported yet"
            )));
        }
        None => {
            return Err(AppError::BadRequest(format!(
                "Instance {id} declares no loader"
            )));
        }
    };
    let loader_version = instance
        .loader_version
        .clone()
        .ok_or_else(|| AppError::BadRequest(format!("Instance {id} declares no loader version")))?;

    let state_clone = state.clone();
    let game_dir = PathBuf::from(&instance.game_dir);
    let mc_version = instance.game_version.clone();
    let instance_id = instance.id.clone();
    tokio::spawn(async move {
        let _operation = operation;
        background_loader_install(
            state_clone,
            instance_id,
            game_dir,
            mc_version,
            loader_kind,
            loader_version,
        )
        .await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateInstanceResponse { instance_id: id }),
    ))
}

/// Background task running a loader-only install for an existing instance.
///
/// Drives the public loader step directly (no full reinstall), drains the
/// queued library downloads, then records `loader_profile`. Emits
/// `InstallingLoader` progress and completion/error events; failures keep
/// status `"ready"`.
async fn background_loader_install(
    state: Arc<AppState>,
    instance_id: String,
    game_dir: PathBuf,
    mc_version: String,
    loader_kind: LoaderKind,
    loader_version: String,
) {
    let fail = |state: &Arc<AppState>, error: String| {
        tracing::error!("Loader install failed: {error}");
        let _ = state.event_tx.send(AppEvent::InstanceError {
            instance_id: instance_id.clone(),
            error,
        });
    };

    let mut downloader = Downloader::new();
    let mut tracker = PhaseProgressTracker::default();
    let requester = reqwest::Client::new();
    let mut ctx = LoaderCtx {
        instance_dir: &game_dir,
        mc_version: &mc_version,
        loader_version: &loader_version,
        downloader: &mut downloader,
        requester: &requester,
    };
    let profile_id = match loader_kind {
        LoaderKind::Fabric => FabricStep.install(&mut ctx).await,
        LoaderKind::Quilt => QuiltStep.install(&mut ctx).await,
        _ => {
            fail(
                &state,
                format!("{loader_kind:?} packs are not supported yet"),
            );
            return;
        }
    };
    let profile_id = match profile_id {
        Ok(id) => id,
        Err(e) => {
            fail(&state, e.to_string());
            return;
        }
    };

    loop {
        match downloader.progress().await {
            Ok(DownloadState::Completed) => break,
            Ok(_) => {
                let phase = RinthInstallState::InstallingLoader.as_str().to_string();
                let remaining = downloader.requests_left();
                let total = tracker.update(&phase, remaining);
                let _ = state.event_tx.send(AppEvent::InstanceProgress {
                    instance_id: instance_id.clone(),
                    phase,
                    remaining,
                    total,
                });
            }
            Err(e) => {
                fail(&state, e.to_string());
                return;
            }
        }
    }

    {
        let db = state.db.lock().unwrap();
        let _ = db::instances::update_loader_profile(&db, &instance_id, &profile_id);
    }
    let _ = state.event_tx.send(AppEvent::InstanceCompleted {
        instance_id: instance_id.clone(),
    });
}
