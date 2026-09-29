use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::sync::oneshot;
use tracing::info;
use uranium_rs::minecraft::verify::InstallationVerifier;
use utoipa::ToSchema;

use crate::db;
use crate::db::instances::InstanceStatus;
use crate::error::AppError;
use crate::events::AppEvent;
use crate::launcher;
use crate::state::{AppState, RunningGame};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/launch/{id}", post(launch_instance))
        .route("/terminate/{id}", post(terminate_instance))
        .route("/running", get(list_running))
        .route("/verify/{id}", get(verify_instance))
}

/// Response returned by [`launch_instance`] on success.
#[derive(Debug, Serialize, ToSchema)]
pub struct LaunchResponse {
    /// OS process ID of the spawned Java process.
    pid: u32,
}

/// Response returned by [`list_running`].
#[derive(Debug, Serialize, ToSchema)]
pub struct RunningResponse {
    /// List of currently running game instances.
    running: Vec<RunningEntry>,
}

/// Response returned by [`verify_instance`].
#[derive(Debug, Serialize, ToSchema)]
pub struct VerificationResponse {
    pub wrong_files: Vec<String>,
}

/// A single entry in the running instances list.
#[derive(Debug, Serialize, ToSchema)]
pub struct RunningEntry {
    /// UUID of the running instance.
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    instance_id: String,
    /// OS process ID of the Java process.
    pid: u32,
}

/// `POST /launch/{id}` — launch a Minecraft instance.
///
/// The instance must have `"ready"` status and must not already be running.
/// Modpack instances with a declared loader additionally require an installed
/// loader profile (`loader_profile`), otherwise launch is rejected with 400.
///
/// **Launch flow** (each step lives in [`crate::launcher`]):
/// 1. Validates the instance is ready (and its loader installed) and not
///    already running
/// 2. Reads and merges the version profile (`versions/<profile>/<profile>.json`,
///    following `inheritsFrom` for loader profiles)
/// 3. Ensures the Mojang Java runtime exists (downloads it if missing)
/// 4. Assembles all arguments into a `LaunchConfig` (classpath, JVM args,
///    game args, memory)
/// 5. Builds and spawns the `java` process with piped stdout/stderr
/// 6. Registers the process in the running map, broadcasts `game:started`,
///    and spawns the background I/O task that broadcasts `game:output` /
///    `game:exited` events
///
/// Returns 400 if the instance is not ready, or 404 if it doesn't exist.
#[utoipa::path(
    post,
    path = "/launch/{id}",
    tag = "launcher",
    params(
        ("id" = String, Path, description = "Instance UUID"),
    ),
    responses(
        (status = 200, description = "Game launched successfully", body = LaunchResponse),
        (status = 400, description = "Instance not ready or already running"),
        (status = 404, description = "Instance not found"),
    )
)]
async fn launch_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<LaunchResponse>, AppError> {
    let instance = launcher::validate_launchable(&state, &id)?;
    let game_dir = std::path::PathBuf::from(&instance.game_dir);

    let profile = instance
        .loader_profile
        .as_deref()
        .unwrap_or(&instance.game_version);
    let (root, jar_id) = launcher::load_merged_root(&game_dir, profile)?;
    let config = launcher::build_launch_config(&instance, &root, &jar_id).await?;

    let mut cmd = launcher::build_java_command(&config);
    let std_cmd = cmd.as_std();
    let argv = std::iter::once(std_cmd.get_program())
        .chain(std_cmd.get_args())
        .map(|a| a.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("\n  ");
    info!("[CMD]:\n  {argv}");
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Internal(format!("Failed to spawn Java process: {e}")))?;
    let pid = child.id().unwrap_or(0);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Internal("Failed to capture stdout from Java process".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Internal("Failed to capture stderr from Java process".into()))?;

    let (kill_tx, kill_rx) = oneshot::channel();

    {
        let mut running = state.running.lock().unwrap();
        running.insert(
            id.clone(),
            RunningGame {
                pid,
                started_at: Instant::now(),
                kill_tx: Some(kill_tx),
            },
        );
    }

    let _ = state.event_tx.send(AppEvent::GameStarted {
        instance_id: id.clone(),
        pid,
    });

    let state_clone = state.clone();
    let id_clone = id.clone();
    let started_at = Instant::now();
    tokio::spawn(async move {
        game_io_task(
            state_clone,
            id_clone,
            child,
            stdout,
            stderr,
            started_at,
            kill_rx,
        )
        .await;
    });

    Ok(Json(LaunchResponse { pid }))
}

#[utoipa::path(
    post,
    path = "/verify/{id}",
    tag = "launcher",
    params(
        ("id" = String, Path, description = "Instance UUID"),
    ),
    responses(
        (status = 200, description = "Verification executed successfully", body = LaunchResponse),
        (status = 400, description = "Instance not ready or running"),
        (status = 404, description = "Instance not found"),
    )
)]
async fn verify_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<VerificationResponse>, AppError> {
    let instance = {
        let db = state.db.lock().unwrap();
        db::instances::get(&db, &id)?
            .ok_or_else(|| AppError::NotFound(format!("Instance {id} not found")))?
    };

    if instance.status != InstanceStatus::Ready {
        return Err(AppError::BadRequest(format!(
            "Instance {id} is not ready (status: {})",
            instance.status
        )));
    }

    {
        let running = state.running.lock().unwrap();
        if running.contains_key(&id) {
            return Err(AppError::BadRequest(format!("Instance {id} is running")));
        }
    }

    let verifier = InstallationVerifier::new(
        &std::path::PathBuf::from(&instance.game_dir),
        &instance.game_version,
    )
    .await;

    if let Err(err) = verifier {
        return Err(AppError::Uranium(err));
    }

    let verifier = verifier.unwrap();
    let results = verifier.verify();

    let mut wrong_files = vec![];

    for obj in results.objects {
        wrong_files.push(obj.hash.clone());
    }

    for lib in results.libs {
        wrong_files.push(lib.name.clone());
    }

    if let Some(client) = results.client {
        wrong_files.push(client.sha1.clone());
    }

    if let Some(index) = results.index {
        wrong_files.push(index.id.clone());
    }

    Ok(VerificationResponse { wrong_files }.into())
}

/// Background task that reads stdout/stderr lines from a running Minecraft
/// process, broadcasts them as [`AppEvent::GameOutput`] events, and waits for
/// the process to exit.
///
/// When the process exits (naturally or via kill signal), updates the database
/// with accumulated playtime, removes the entry from the running map, and
/// broadcasts [`AppEvent::GameExited`].
async fn game_io_task(
    state: Arc<AppState>,
    instance_id: String,
    mut child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    started_at: Instant,
    kill_rx: oneshot::Receiver<()>,
) {
    let mut read_stdout = tokio::spawn(read_pipe_lines(
        state.clone(),
        instance_id.clone(),
        "stdout",
        stdout,
    ));
    let mut read_stderr = tokio::spawn(read_pipe_lines(
        state.clone(),
        instance_id.clone(),
        "stderr",
        stderr,
    ));

    tokio::pin! {
        let kill_rx = kill_rx;
    }

    let was_killed: bool = tokio::select! {
        biased;
        res = &mut kill_rx => res.is_ok(),
        _ = async {
            let _ = (&mut read_stdout).await;
            let _ = (&mut read_stderr).await;
        } => false,
    };

    if !was_killed {
        read_stdout.abort();
        read_stderr.abort();
    }

    if was_killed {
        let _ = child.kill().await;
    }
    let exit_status = child.wait().await;
    let exit_code = exit_status.ok().and_then(|s| s.code()).unwrap_or(-1);
    let playtime = started_at.elapsed().as_secs();

    info!("Player exit the game, time played {}", playtime);
    let now = chrono::Utc::now().to_rfc3339();
    {
        let db = state.db.lock().unwrap();
        let _ = db::instances::update_playtime(&db, &instance_id, playtime as i64);
        let _ = db::instances::update_last_played(&db, &instance_id, &now);
    }
    {
        let mut running = state.running.lock().unwrap();
        running.remove(&instance_id);
    }

    let _ = state.event_tx.send(AppEvent::GameExited {
        instance_id,
        exit_code,
        playtime_seconds: playtime,
    });
}

/// Read lines from a pipe (stdout or stderr) and broadcast them as
/// [`AppEvent::GameOutput`] events.
async fn read_pipe_lines<R>(
    state: Arc<AppState>,
    instance_id: String,
    stream: &'static str,
    pipe: R,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut reader = tokio::io::BufReader::new(pipe);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line
                    .trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .to_string();
                let _ = state.event_tx.send(AppEvent::GameOutput {
                    instance_id: instance_id.clone(),
                    stream: stream.to_string(),
                    line: trimmed,
                });
            }
            Err(_) => break,
        }
    }
}

/// `POST /terminate/{id}` — terminate a running game instance.
///
/// Sends a kill signal to the running game process. The background
/// `game_io_task` handles killing the process, updating the database, and
/// broadcasting [`AppEvent::GameExited`]. Returns 404 if the instance is
/// not currently running.
#[utoipa::path(
    post,
    path = "/terminate/{id}",
    tag = "launcher",
    params(
        ("id" = String, Path, description = "Instance UUID"),
    ),
    responses(
        (status = 200, description = "Game terminated successfully"),
        (status = 404, description = "Instance not running"),
    )
)]
async fn terminate_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut running = state.running.lock().unwrap();
    let entry = running
        .get_mut(&id)
        .ok_or_else(|| AppError::NotFound(format!("Instance {id} is not running")))?;

    if let Some(tx) = entry.kill_tx.take() {
        let _ = tx.send(());
    }

    Ok(Json(serde_json::json!({ "terminated": id })))
}

/// `GET /running` — list all currently running game instances.
///
/// Returns an array of `{instance_id, pid}` objects. An empty array means
/// no games are running.
#[utoipa::path(
    get,
    path = "/running",
    tag = "launcher",
    responses(
        (status = 200, description = "List of running instances", body = RunningResponse),
    )
)]
async fn list_running(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RunningResponse>, AppError> {
    let running = state.running.lock().unwrap();
    let entries: Vec<RunningEntry> = running
        .iter()
        .map(|(id, game)| RunningEntry {
            instance_id: id.clone(),
            pid: game.pid,
        })
        .collect();
    Ok(Json(RunningResponse { running: entries }))
}
