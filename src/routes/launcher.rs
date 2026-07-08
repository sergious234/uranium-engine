use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::sync::oneshot;
use tracing::info;
use uranium_rs::mine_data_structs::minecraft::{self, Root, Rule};
use uranium_rs::version_checker::InstallationVerifier;
use utoipa::ToSchema;

use crate::db;
use crate::db::instances::InstanceStatus;
use crate::error::AppError;
use crate::events::AppEvent;
use crate::paths;
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
///
/// **Launch flow:**
/// 1. Reads the Minecraft version JSON (`{game_dir}/versions/{v}/{v}.json`)
/// 2. Downloads the Mojang Java runtime via [`RuntimeDownloader`] if not cached
/// 3. Builds the classpath from the version's libraries (OS-filtered)
/// 4. Resolves game arguments with token substitution
/// 5. Spawns `java` as a child process with piped stdout/stderr
/// 6. Registers the process in the running map, broadcasts `game:started`
/// 7. Spawns a background task that reads output lines, broadcasts
///    `game:output` events, and on exit broadcasts `game:exited`
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
            return Err(AppError::BadRequest(format!(
                "Instance {id} is already running"
            )));
        }
    }

    let game_dir = PathBuf::from(&instance.game_dir);
    let version = instance.game_version.clone();
    let version_json_path = game_dir
        .join("versions")
        .join(&version)
        .join(format!("{version}.json"));

    let version_json_str = std::fs::read_to_string(&version_json_path)
        .map_err(|e| AppError::Internal(format!("Failed to read version JSON: {e}")))?;
    let root: Root = serde_json::from_str(&version_json_str)
        .map_err(|e| AppError::Internal(format!("Failed to parse version JSON: {e}")))?;

    let os = std::env::consts::OS;
    let component = &root.java_version.component;
    let minecraft_home = minecraft::get_minecraft_path()
        .ok_or_else(|| AppError::Internal("Cannot determine .minecraft path".into()))?;
    let java_bin = minecraft_home
        .join("runtime")
        .join(component)
        .join(os)
        .join(component)
        .join("bin")
        .join("java");

    if !java_bin.exists() {
        let mut runtime_downloader =
            uranium_rs::downloaders::RuntimeDownloader::new(component.clone());
        runtime_downloader
            .download()
            .await
            .map_err(|e| AppError::Internal(format!("Failed to download runtime: {e}")))?;
    }

    let java_bin_str = java_bin.to_string_lossy().to_string();
    let classpath = build_classpath(&game_dir, &root)?;
    let main_class = root.main_class.clone();
    let version_type = root.version_type.clone();

    let max_memory = load_max_memory();
    let mut jvm_args = resolve_jvm_arguments(&root, &game_dir, &version, &version_type);
    jvm_args.retain(|s| !s.is_empty());

    let mut cmd = Command::new(&java_bin_str);
    cmd.arg(&max_memory);

    if std::env::vars().any(|(k, _v)| k == "JVM_ARGS_ON") {
        for arg in &jvm_args {
            cmd.arg(arg);
        }
    }

    cmd.arg("-cp").arg(&classpath).arg(&main_class);

    let mut game_args = resolve_game_arguments(&root, &game_dir, &version, &version_type);
    game_args.retain(|s| !s.is_empty());
    for arg in &game_args {
        cmd.arg(arg);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    info!("[CMD]: {cmd:#?}");

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

    let verifier =
        InstallationVerifier::new(&PathBuf::from(&instance.game_dir), &instance.game_version).await;

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

/// Build a Java classpath string from the version's libraries.
///
/// Includes the version JAR and all library artifacts that pass OS filtering.
/// Paths are joined with `:` (Unix separator). Only existing files on disk are
/// included.
fn build_classpath(game_dir: &std::path::Path, root: &Root) -> Result<String, AppError> {
    let version_jar = game_dir
        .join("versions")
        .join(&root.id)
        .join(format!("{}.jar", root.id));

    let mut paths = vec![version_jar.to_string_lossy().to_string()];

    for lib in root.libraries.iter() {
        if let Some(rel_path) = lib.get_rel_path()
            && lib.applies()
        {
            let lib_path = game_dir.join("libraries").join(rel_path);
            if lib_path.exists() {
                paths.push(lib_path.to_string_lossy().to_string());
            }

            // There are libs that have a duplicate download field and the actual lib (a native
            // library usually) yields under the classifier.
            // https://piston-meta.mojang.com/v1/packages/e0e7ab5ed6f55bbd874ef95be3c9356d67e64b57/1.17.1.json
            if let Some(classifier) = lib.get_os_classifier() {
                let lib_path = game_dir.join("libraries").join(&classifier.path);
                if lib_path.exists() {
                    paths.push(lib_path.to_string_lossy().to_string());
                }
            }
        }
    }

    Ok(paths.join(":"))
}

/// Resolve game arguments from the version's [`Arguments`] struct.
///
/// Applies OS-specific rules (allow/disallow) and performs token substitution
/// on each argument value. Returns a flat list of argument strings ready to
/// pass to the Java process after the main class.
fn resolve_game_arguments(
    root: &Root,
    game_dir: &std::path::Path,
    version: &str,
    version_type: &str,
) -> Vec<String> {
    let mut args = Vec::new();
    let asset_index = &root.asset_index.id;

    for arg in root.arguments.game.iter() {
        match arg {
            minecraft::GameArgument::String(s) => {
                args.push(substitute_tokens(
                    s,
                    game_dir,
                    version,
                    version_type,
                    asset_index,
                ));
                info!("Pushed single arg: {s}")
            }
            minecraft::GameArgument::Object { rules, value } => {
                let allowed = rules.iter().all(Rule::applies);

                if allowed {
                    match value {
                        minecraft::ValueType::Single(s) => {
                            args.push(substitute_tokens(
                                s,
                                game_dir,
                                version,
                                version_type,
                                asset_index,
                            ));
                            info!("Pushed single arg object: {s}")
                        }
                        minecraft::ValueType::Multiple(v) => {
                            for s in v.iter() {
                                args.push(substitute_tokens(
                                    s,
                                    game_dir,
                                    version,
                                    version_type,
                                    asset_index,
                                ));
                                info!("Pushed multiple arg object: {s}")
                            }
                        }
                    }
                }
            }
        }
    }

    args
}

fn resolve_jvm_arguments(
    root: &Root,
    game_dir: &std::path::Path,
    version: &str,
    version_type: &str,
) -> Vec<String> {
    let mut args = Vec::new();
    let asset_index = &root.asset_index.id;

    for arg in root.arguments.jvm.iter() {
        match arg {
            minecraft::GameArgument::String(s) => {
                args.push(substitute_tokens(
                    s,
                    game_dir,
                    version,
                    version_type,
                    asset_index,
                ));
                info!("Pushed single arg: {s}")
            }
            minecraft::GameArgument::Object { rules, value } => {
                let allowed = rules.iter().all(Rule::applies);

                if allowed {
                    match value {
                        minecraft::ValueType::Single(s) => {
                            args.push(substitute_tokens(
                                s,
                                game_dir,
                                version,
                                version_type,
                                asset_index,
                            ));
                            info!("Pushed single arg object: {s}")
                        }
                        minecraft::ValueType::Multiple(v) => {
                            for s in v.iter() {
                                args.push(substitute_tokens(
                                    s,
                                    game_dir,
                                    version,
                                    version_type,
                                    asset_index,
                                ));
                                info!("Pushed multiple arg object: {s}")
                            }
                        }
                    }
                }
            }
        }
    }

    args
}

/// Substitute known `${...}` tokens in a Minecraft game argument string.
///
/// Supported tokens:
/// - `${game_directory}`, `${assets_root}`, `${assets_index_name}`
/// - `${auth_player_name}`, `${auth_uuid}`, `${auth_access_token}`
/// - `${user_properties}`, `${user_type}`
/// - `${version_name}`, `${version_type}`
/// - `${launcher_name}`, `${launcher_version}`
/// - `${natives_directory}`, `${library_directory}`
/// - `${classpath_separator}`, `${classpath}`
///
/// Unknown tokens are left as-is.
fn substitute_tokens(
    s: &str,
    game_dir: &std::path::Path,
    version: &str,
    version_type: &str,
    asset_index: &str,
) -> String {
    let assets_root = game_dir.join("assets");
    s.replace("${game_directory}", &game_dir.to_string_lossy())
        .replace("${assets_root}", &assets_root.to_string_lossy())
        .replace("${assets_index_name}", asset_index)
        .replace("${auth_player_name}", "player1")
        .replace("${auth_uuid}", "00000000-0000-0000-0000-000000000000")
        .replace("${auth_access_token}", "0")
        .replace("${user_properties}", "{}")
        .replace("${user_type}", "msa")
        .replace("--quickPlayPath", "")
        .replace("${quickPlayPath}", "")
        .replace("--quickPlaySingleplayer", "")
        .replace("${quickPlaySingleplayer}", "")
        .replace("--quickPlayMultiplayer", "")
        .replace("${quickPlayMultiplayer}", "")
        .replace("--quickPlayRealms", "")
        .replace("${quickPlayRealms}", "")
        .replace("--demo", "")
        .replace("${version_name}", version)
        .replace("${version_type}", version_type)
        .replace("${launcher_name}", "uranium-engine")
        .replace("${launcher_version}", "0.1.0")
        .replace(
            "${natives_directory}",
            &game_dir.join("natives").to_string_lossy(),
        )
        .replace(
            "${library_directory}",
            &game_dir.join("libraries").to_string_lossy(),
        )
        .replace("${classpath_separator}", ":")
        .replace("${classpath}", "")
        .replace("-cp", "")
}

/// Read the `max_memory` setting from `config.toml`.
///
/// Falls back to `"-Xmx4G"` if the config file does not exist or the key is
/// missing.
fn load_max_memory() -> String {
    let config_path = paths::config_file();
    if let Ok(content) = std::fs::read_to_string(&config_path)
        && let Ok(settings) = toml::from_str::<HashMap<String, toml::Value>>(&content)
        && let Some(toml::Value::String(mem)) = settings.get("max_memory")
    {
        return format!("-Xmx{mem}");
    }
    "-Xmx4G".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_substitute_game_directory() {
        let result = substitute_tokens(
            "${game_directory}/options.txt",
            &PathBuf::from("/home/user/mc"),
            "1.21",
            "release",
            "19",
        );
        assert_eq!(result, "/home/user/mc/options.txt");
    }

    #[test]
    fn test_substitute_auth_tokens() {
        let result = substitute_tokens(
            "--username ${auth_player_name} --uuid ${auth_uuid}",
            &PathBuf::from("/mc"),
            "1.21",
            "release",
            "19",
        );
        assert_eq!(
            result,
            "--username Player --uuid 00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn test_substitute_assets_root() {
        let result = substitute_tokens(
            "--assetsDir ${assets_root}",
            &PathBuf::from("/mc"),
            "1.21",
            "release",
            "19",
        );
        assert_eq!(result, "--assetsDir /mc/assets");
    }

    #[test]
    fn test_substitute_all_common_tokens() {
        let result = substitute_tokens(
            "--gameDir ${game_directory} --assetsDir ${assets_root} --assetIndex ${assets_index_name} --username ${auth_player_name} --uuid ${auth_uuid} --accessToken ${auth_access_token} --userType ${user_type} --version ${version_name} --versionType ${version_type}",
            &PathBuf::from("/mc"),
            "1.21.4",
            "release",
            "32",
        );
        assert!(result.contains("--gameDir /mc"));
        assert!(result.contains("--assetIndex 32"));
        assert!(result.contains("--username Player"));
        assert!(result.contains("--accessToken 0"));
        assert!(result.contains("--userType msa"));
        assert!(result.contains("--version 1.21.4"));
        assert!(result.contains("--versionType release"));
    }

    #[test]
    fn test_substitute_natives_dir() {
        let result = substitute_tokens(
            "-Djava.library.path=${natives_directory}",
            &PathBuf::from("/mc"),
            "1.21",
            "release",
            "19",
        );
        assert_eq!(result, "-Djava.library.path=/mc/versions/1.21/1.21-natives");
    }

    #[test]
    fn test_substitute_library_directory() {
        let result = substitute_tokens(
            "${library_directory}",
            &PathBuf::from("/mc"),
            "1.21",
            "release",
            "19",
        );
        assert_eq!(result, "/mc/libraries");
    }

    #[test]
    fn test_substitute_untouched_unknown_token_stays() {
        let result = substitute_tokens(
            "${custom_token}",
            &PathBuf::from("/mc"),
            "1.21",
            "release",
            "19",
        );
        assert_eq!(result, "${custom_token}");
    }

    #[test]
    fn test_substitute_classpath() {
        let result = substitute_tokens(
            "-cp ${classpath}",
            &PathBuf::from("/mc"),
            "1.21",
            "release",
            "19",
        );
        assert_eq!(result, "-cp ");
    }

    #[test]
    fn test_load_max_memory_default() {
        let result = load_max_memory();
        // Should always start with -Xmx followed by a non-empty value
        assert!(
            result.starts_with("-Xmx"),
            "Expected -Xmx prefix, got {result}"
        );
        assert!(result.len() > 4, "Expected memory value after -Xmx");
    }

    fn minimal_root(id: &str) -> Root {
        let json = serde_json::json!({
            "id": id,
            "mainClass": "net.minecraft.client.main.Main",
            "type": "release",
            "assetIndex": { "id": "19", "sha1": "x", "size": 1, "totalSize": 1, "url": "" },
            "assets": "19",
            "downloads": {},
            "javaVersion": { "component": "java-runtime-delta", "majorVersion": 21 },
            "libraries": [],
            "arguments": { "game": [] }
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn test_build_classpath_minimal() {
        let root = minimal_root("1.21");
        let game_dir = PathBuf::from("/test/mc");
        let result = build_classpath(&game_dir, &root).unwrap();
        assert_eq!(result, "/test/mc/versions/1.21/1.21.jar");
    }

    #[test]
    fn test_build_classpath_nonexistent_dir() {
        let root = minimal_root("1.0");
        let game_dir = PathBuf::from("/nonexistent");
        let result = build_classpath(&game_dir, &root).unwrap();
        assert_eq!(result, "/nonexistent/versions/1.0/1.0.jar");
    }
}
