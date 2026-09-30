//! Minecraft launch pipeline.
//!
//! Each step of launching a game lives in a single-responsibility function:
//!
//! 1. [`validate_launchable`] — DB fetch + status/loader/duplicate checks
//! 2. [`load_merged_root`] — read a version profile and flatten its
//!    `inheritsFrom` chain (loader profiles inherit the vanilla JSON)
//! 3. [`ensure_java_runtime`] — resolve the Java binary, download if missing
//! 4. [`build_launch_config`] — assemble every argument into a [`LaunchConfig`]
//! 5. [`build_java_command`] — turn a [`LaunchConfig`] into a [`Command`]
//!
//! Adding new launch parameters (per-instance java args, quickplay, auth,
//! ...) means adding a field to [`LaunchConfig`] and appending it in
//! [`build_launch_config`] or [`build_java_command`] — nothing else changes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tokio::process::Command;
use tracing::info;
use uranium_rs::engine::Downloader;
use uranium_rs::mine_data_structs::minecraft::{self, Root, Rule};
use uranium_rs::minecraft::runtime::RuntimeDownloader;

use crate::db;
use crate::db::instances::InstanceStatus;
use crate::error::AppError;
use crate::routes::settings::{Settings, load_settings};
use crate::state::AppState;

/// Window resolution used for `${resolution_*}` token substitution.
#[derive(Copy, Clone, Debug)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// Context used for `${...}` token substitution in version arguments.
struct TokenContext<'a> {
    game_dir: &'a Path,
    version: &'a str,
    version_type: &'a str,
    asset_index: &'a str,
    resolution: Resolution,
}

/// Everything needed to spawn a Minecraft process.
///
/// This is the single extension point for new launch parameters.
#[derive(Debug)]
pub struct LaunchConfig {
    pub java_bin: String,
    /// Working directory for Minecraft and loader-generated files.
    pub working_dir: PathBuf,
    pub classpath: String,
    pub main_class: String,
    /// JVM arguments resolved from the version JSON (applied only when the
    /// `JVM_ARGS_ON` env var is set).
    pub jvm_args: Vec<String>,
    /// Game arguments resolved from the version JSON.
    pub game_args: Vec<String>,
    /// e.g. `"-Xmx4G"`.
    pub max_memory: String,
    pub resolution: Resolution,
}

/// Validate that an instance exists, is ready, and is not already running.
///
/// Returns the instance on success.
pub fn validate_launchable(
    state: &Arc<AppState>,
    id: &str,
) -> Result<db::instances::Instance, AppError> {
    let instance = {
        let db = state.db.lock().unwrap();
        db::instances::get(&db, id)?
            .ok_or_else(|| AppError::NotFound(format!("Instance {id} not found")))?
    };

    if instance.status != InstanceStatus::Ready {
        return Err(AppError::BadRequest(format!(
            "Instance {id} is not ready (status: {})",
            instance.status
        )));
    }

    if instance.loader.is_some() && instance.loader_profile.is_none() {
        return Err(AppError::BadRequest(format!(
            "Instance {id} loader not installed"
        )));
    }

    {
        let running = state.running.lock().unwrap();
        if running.contains_key(id) {
            return Err(AppError::BadRequest(format!(
                "Instance {id} is already running"
            )));
        }
    }

    Ok(instance)
}

/// Read and parse `{game_dir}/versions/{version}/{version}.json`.
pub fn load_version_json(game_dir: &Path, version: &str) -> Result<Root, AppError> {
    let path = game_dir
        .join("versions")
        .join(version)
        .join(format!("{version}.json"));

    let content = std::fs::read_to_string(&path)
        .map_err(|e| AppError::Internal(format!("Failed to read version JSON: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| AppError::Internal(format!("Failed to parse version JSON: {e}")))
}

/// Maximum `inheritsFrom` chain depth (cycle protection).
const MAX_INHERIT_DEPTH: usize = 8;

/// Load a version profile and flatten its `inheritsFrom` chain.
///
/// Loader profiles (e.g. `fabric-loader-0.16.9-1.21`) carry only their own
/// libraries, `mainClass` and `arguments`, inheriting everything else from
/// the vanilla version. Since those profiles omit fields `Root` requires
/// (`assetIndex`, `downloads`, `javaVersion`), the merge happens at
/// `serde_json::Value` level and deserializes once.
///
/// Overlay rules (oldest ancestor → child):
/// - `libraries`: child first + parent entries deduped by `name`
///   (child wins, matching first-on-classpath semantics)
/// - `mainClass`: child's if non-empty (loader wins)
/// - `arguments.game` / `arguments.jvm`: per-section, child's if non-empty
/// - any other key: child's if present and non-empty, else parent's
/// - `id`: the requested profile id
///
/// Returns the merged root plus the jar owner id: the nearest chain member
/// declaring `downloads.client` (loader profiles ship no jar).
pub fn load_merged_root(game_dir: &Path, profile: &str) -> Result<(Root, String), AppError> {
    // -- Walk the chain (profile-first)
    let mut chain: Vec<serde_json::Value> = Vec::new();
    let mut seen = HashSet::new();
    let mut current = profile.to_string();
    loop {
        if !seen.insert(current.clone()) {
            return Err(AppError::Internal(format!(
                "Version inheritance cycle at {current}"
            )));
        }
        if chain.len() >= MAX_INHERIT_DEPTH {
            return Err(AppError::Internal(format!(
                "Version inheritance too deep at {current}"
            )));
        }
        let path = game_dir
            .join("versions")
            .join(&current)
            .join(format!("{current}.json"));
        let content = std::fs::read_to_string(&path)
            .map_err(|e| AppError::Internal(format!("Failed to read version JSON: {e}")))?;
        let value: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| AppError::Internal(format!("Failed to parse version JSON: {e}")))?;
        let parent = value
            .get("inheritsFrom")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        chain.push(value);
        match parent {
            Some(next) => current = next,
            None => break,
        }
    }

    // -- Overlay oldest ancestor → profile
    let mut merged = chain.pop().unwrap();
    let mut jar_id = profile.to_string();
    if has_client_download(&merged) {
        jar_id = chain_id(&merged).unwrap_or_else(|| profile.to_string());
    }
    // chain is profile-first minus the popped oldest; iterate oldest→profile
    for member in chain.iter().rev() {
        overlay_version(&mut merged, member);
        if has_client_download(member) {
            jar_id = chain_id(member).unwrap_or_else(|| profile.to_string());
        }
    }
    if let Some(map) = merged.as_object_mut() {
        map.insert(
            "id".to_string(),
            serde_json::Value::String(profile.to_string()),
        );
        map.remove("inheritsFrom");
    }

    let root: Root = serde_json::from_value(merged)
        .map_err(|e| AppError::Internal(format!("Failed to merge version JSON: {e}")))?;
    Ok((root, jar_id))
}

/// Whether a version JSON declares a client jar download.
fn has_client_download(value: &serde_json::Value) -> bool {
    value
        .get("downloads")
        .and_then(|d| d.get("client"))
        .map(|c| !is_empty_json(c))
        .unwrap_or(false)
}

/// The `id` field of a version JSON, if present.
fn chain_id(value: &serde_json::Value) -> Option<String> {
    value.get("id").and_then(|v| v.as_str()).map(str::to_string)
}

/// Overlay `child` onto `parent` (both version JSON values).
fn overlay_version(parent: &mut serde_json::Value, child: &serde_json::Value) {
    let (Some(pmap), Some(cmap)) = (parent.as_object_mut(), child.as_object()) else {
        if !is_empty_json(child) {
            *parent = child.clone();
        }
        return;
    };

    // -- Libraries: child first + parent deduped by name
    if let Some(clibs) = cmap.get("libraries").and_then(|v| v.as_array()) {
        let mut merged_libs = clibs.clone();
        let mut names: HashSet<&str> = clibs
            .iter()
            .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
            .collect();
        if let Some(plibs) = pmap.get("libraries").and_then(|v| v.as_array()) {
            for lib in plibs {
                let dominated = lib
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| names.contains(n))
                    .unwrap_or(false);
                if dominated {
                    continue;
                }
                if let Some(name) = lib.get("name").and_then(|n| n.as_str()) {
                    names.insert(name);
                }
                merged_libs.push(lib.clone());
            }
        }
        pmap.insert(
            "libraries".to_string(),
            serde_json::Value::Array(merged_libs),
        );
    }

    // -- Arguments: per-section overlay
    if let Some(cargs) = cmap.get("arguments") {
        match pmap.get_mut("arguments") {
            Some(pargs) => overlay_arguments(pargs, cargs),
            None => {
                if !is_empty_json(cargs) {
                    pmap.insert("arguments".to_string(), cargs.clone());
                }
            }
        }
    }

    // -- Passthrough keys: child wins if present and non-empty
    for (key, value) in cmap {
        if key == "libraries" || key == "arguments" || key == "id" || key == "inheritsFrom" {
            continue;
        }
        if !is_empty_json(value) {
            pmap.insert(key.clone(), value.clone());
        }
    }
}

/// Overlay one `arguments` object per `game`/`jvm` section.
fn overlay_arguments(parent: &mut serde_json::Value, child: &serde_json::Value) {
    let (Some(pmap), Some(cmap)) = (parent.as_object_mut(), child.as_object()) else {
        if !is_empty_json(child) {
            *parent = child.clone();
        }
        return;
    };
    for section in ["game", "jvm"] {
        if let Some(child_section) = cmap.get(section)
            && !is_empty_json(child_section)
        {
            pmap.insert(section.to_string(), child_section.clone());
        }
    }
}

/// `true` for JSON null, `""`, `[]`, and `{}` (treated as "absent" in merges).
fn is_empty_json(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// Resolve the Mojang Java runtime binary for the version, downloading it via
/// [`uranium_rs::downloaders::RuntimeDownloader`] if it is not cached yet.
pub async fn ensure_java_runtime(root: &Root) -> Result<PathBuf, AppError> {
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
        let mut runtime_downloader = RuntimeDownloader::<Downloader>::new(component.clone());
        runtime_downloader
            .start()
            .await
            .map_err(|e| AppError::Internal(format!("Failed to download runtime: {e}")))?;
    }

    Ok(java_bin)
}

/// Assemble all launch parameters into a [`LaunchConfig`].
pub async fn build_launch_config(
    instance: &db::instances::Instance,
    root: &Root,
    jar_id: &str,
) -> Result<LaunchConfig, AppError> {
    let settings = load_settings()?;
    let java_bin = if let Some(path) = configured_java(&instance.java_runtime, &settings) {
        path
    } else {
        ensure_java_runtime(root)
            .await?
            .to_string_lossy()
            .to_string()
    };
    let game_dir = PathBuf::from(&instance.game_dir);
    let version = &instance.game_version;
    let version_type = root.version_type.clone();
    let resolution = load_resolution(&settings);

    let mut jvm_args = resolve_arguments(
        &root.arguments.jvm,
        &TokenContext {
            game_dir: &game_dir,
            version,
            version_type: &version_type,
            asset_index: &root.asset_index.id,
            resolution,
        },
    );
    jvm_args.retain(|s| !s.is_empty());
    if let Some(global_args) = &settings.jvm_args {
        jvm_args.extend(
            global_args
                .iter()
                .filter(|arg| !arg.trim().is_empty())
                .cloned(),
        );
    }
    if !instance.java_args.trim().is_empty() {
        let instance_args = shlex::split(&instance.java_args).ok_or_else(|| {
            AppError::BadRequest("Invalid quoting in instance Java arguments".into())
        })?;
        jvm_args.extend(instance_args);
    }

    let mut game_args = resolve_arguments(
        &root.arguments.game,
        &TokenContext {
            game_dir: &game_dir,
            version,
            version_type: &version_type,
            asset_index: &root.asset_index.id,
            resolution,
        },
    );
    game_args.retain(|s| !s.is_empty());

    Ok(LaunchConfig {
        java_bin,
        working_dir: game_dir.clone(),
        classpath: build_classpath(&game_dir, root, jar_id)?,
        main_class: root.main_class.clone(),
        jvm_args,
        game_args,
        max_memory: format!("-Xmx{}", settings.max_memory.as_deref().unwrap_or("2G")),
        resolution,
    })
}

/// Build the Java process command from a [`LaunchConfig`].
pub fn build_java_command(config: &LaunchConfig) -> Command {
    let mut cmd = Command::new(&config.java_bin);
    cmd.current_dir(&config.working_dir);
    cmd.arg(&config.max_memory);

    for arg in &config.jvm_args {
        cmd.arg(arg);
    }

    cmd.arg("-cp")
        .arg(&config.classpath)
        .arg(&config.main_class);

    for arg in &config.game_args {
        cmd.arg(arg);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

fn configured_java(instance_runtime: &str, settings: &Settings) -> Option<String> {
    let instance_path = instance_runtime.trim();
    if !instance_path.is_empty() && instance_path != "java" {
        return Some(instance_path.to_string());
    }
    settings
        .java_path
        .as_ref()
        .filter(|path| !path.trim().is_empty())
        .cloned()
}

/// Build a Java classpath string from the (possibly merged) version's libraries.
///
/// Includes the owner version JAR (`versions/{jar_id}/{jar_id}.jar`, which is
/// the vanilla jar for loader profiles) and all library artifacts that pass
/// OS filtering. Libraries without `downloads` (hash-less loader libs) fall
/// back to the Maven-coordinate path derived from `name`. Paths are joined
/// with `:` (Unix separator). Only existing files on disk are included.
pub fn build_classpath(game_dir: &Path, root: &Root, jar_id: &str) -> Result<String, AppError> {
    let version_jar = game_dir
        .join("versions")
        .join(jar_id)
        .join(format!("{jar_id}.jar"));

    let mut paths = vec![version_jar.to_string_lossy().to_string()];

    for lib in root.libraries.iter() {
        if !lib.applies() {
            continue;
        }
        let rel_path: Option<PathBuf> = lib
            .get_rel_path()
            .map(Path::to_path_buf)
            .or_else(|| maven_rel_path(&lib.name));
        if let Some(rel_path) = rel_path {
            let lib_path = game_dir.join("libraries").join(rel_path);
            if lib_path.exists() {
                paths.push(lib_path.to_string_lossy().to_string());
            }
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

    Ok(paths.join(":"))
}

/// Derive a `libraries/`-relative path from Maven coordinates
/// (`group:artifact:version[:classifier]`) for profile libraries that omit
/// `downloads`. Returns `None` for malformed coordinates.
fn maven_rel_path(name: &str) -> Option<PathBuf> {
    let mut parts = name.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    let classifier = parts.next();
    if parts.next().is_some() || group.is_empty() || artifact.is_empty() || version.is_empty() {
        return None;
    }
    let mut file = format!("{artifact}-{version}");
    if let Some(c) = classifier.filter(|c| !c.is_empty()) {
        file.push('-');
        file.push_str(c);
    }
    file.push_str(".jar");

    let mut path = PathBuf::new();
    path.extend(group.split('.'));
    path.push(artifact);
    path.push(version);
    path.push(file);
    Some(path)
}

/// Resolve a list of version arguments (`arguments.game` or `arguments.jvm`).
///
/// Applies OS-specific rules (allow/disallow) and performs token substitution
/// on each value.
fn resolve_arguments(args: &[minecraft::GameArgument], ctx: &TokenContext) -> Vec<String> {
    let mut out = Vec::new();

    for arg in args.iter() {
        match arg {
            minecraft::GameArgument::String(s) => {
                out.push(substitute_tokens(s, ctx));
                info!("Pushed single arg: {s}")
            }
            minecraft::GameArgument::Object { rules, value } => {
                let allowed = rules.iter().all(Rule::applies);

                if allowed {
                    match value {
                        minecraft::ValueType::Single(s) => {
                            out.push(substitute_tokens(s, ctx));
                            info!("Pushed single arg object: {s}")
                        }
                        minecraft::ValueType::Multiple(v) => {
                            for s in v.iter() {
                                out.push(substitute_tokens(s, ctx));
                                info!("Pushed multiple arg object: {s}")
                            }
                        }
                    }
                }
            }
        }
    }

    out
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
fn substitute_tokens(s: &str, ctx: &TokenContext) -> String {
    let assets_root = ctx.game_dir.join("assets");
    s.replace("${game_directory}", &ctx.game_dir.to_string_lossy())
        .replace("${assets_root}", &assets_root.to_string_lossy())
        .replace("${assets_index_name}", ctx.asset_index)
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
        .replace("${version_name}", ctx.version)
        .replace("${version_type}", ctx.version_type)
        .replace("${resolution_width}", &ctx.resolution.width.to_string())
        .replace("${resolution_height}", &ctx.resolution.height.to_string())
        .replace("${launcher_name}", "uranium-engine")
        .replace("${launcher_version}", "0.1.0")
        .replace(
            "${natives_directory}",
            &ctx.game_dir.join("natives").to_string_lossy(),
        )
        .replace(
            "${library_directory}",
            &ctx.game_dir.join("libraries").to_string_lossy(),
        )
        .replace("${classpath_separator}", ":")
        .replace("${classpath}", "")
        .replace("-cp", "")
}

fn load_resolution(settings: &Settings) -> Resolution {
    if let (Some(width), Some(height)) = (settings.window_width, settings.window_height) {
        return Resolution { width, height };
    }

    info!("Using fallback 1080p");
    Resolution {
        width: 1920,
        height: 1080,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx<'a>(game_dir: &'a Path, version: &'a str) -> TokenContext<'a> {
        TokenContext {
            game_dir,
            version,
            version_type: "release",
            asset_index: "19",
            resolution: Resolution {
                width: 1920,
                height: 1080,
            },
        }
    }

    #[test]
    fn configured_java_prefers_instance_then_global() {
        let mut settings = Settings::default();
        settings.java_path = Some("/global/java".into());
        assert_eq!(
            configured_java("/instance/java", &settings).as_deref(),
            Some("/instance/java")
        );
        assert_eq!(
            configured_java("java", &settings).as_deref(),
            Some("/global/java")
        );
        settings.java_path = None;
        assert_eq!(configured_java("java", &settings), None);
    }

    #[test]
    fn java_command_includes_official_and_custom_jvm_arguments() {
        let config = LaunchConfig {
            java_bin: "/custom/java".into(),
            working_dir: PathBuf::from("/game"),
            classpath: "/game/client.jar".into(),
            main_class: "net.minecraft.client.main.Main".into(),
            jvm_args: vec![
                "-Djava.library.path=/game/natives".into(),
                "-Dexample=a b".into(),
            ],
            game_args: vec!["--gameDir".into(), "/game".into()],
            max_memory: "-Xmx4G".into(),
            resolution: Resolution {
                width: 854,
                height: 480,
            },
        };
        let command = build_java_command(&config);
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            &args[..4],
            [
                "-Xmx4G",
                "-Djava.library.path=/game/natives",
                "-Dexample=a b",
                "-cp"
            ]
        );
        assert_eq!(command.as_std().get_program(), "/custom/java");
        assert_eq!(command.as_std().get_current_dir(), Some(Path::new("/game")));
    }

    #[test]
    fn test_substitute_game_directory() {
        let dir = PathBuf::from("/home/user/mc");
        let result = substitute_tokens("${game_directory}/options.txt", &ctx(&dir, "1.21"));
        assert_eq!(result, "/home/user/mc/options.txt");
    }

    #[test]
    fn test_substitute_auth_tokens() {
        let dir = PathBuf::from("/mc");
        let result = substitute_tokens(
            "--username ${auth_player_name} --uuid ${auth_uuid}",
            &ctx(&dir, "1.21"),
        );
        assert_eq!(
            result,
            "--username player1 --uuid 00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn test_substitute_assets_root() {
        let dir = PathBuf::from("/mc");
        let result = substitute_tokens("--assetsDir ${assets_root}", &ctx(&dir, "1.21"));
        assert_eq!(result, "--assetsDir /mc/assets");
    }

    #[test]
    fn test_substitute_all_common_tokens() {
        let dir = PathBuf::from("/mc");
        let c = TokenContext {
            game_dir: &dir,
            version: "1.21.4",
            version_type: "release",
            asset_index: "32",
            resolution: Resolution {
                width: 1920,
                height: 1080,
            },
        };
        let result = substitute_tokens(
            "--gameDir ${game_directory} --assetsDir ${assets_root} --assetIndex ${assets_index_name} --username ${auth_player_name} --uuid ${auth_uuid} --accessToken ${auth_access_token} --userType ${user_type} --version ${version_name} --versionType ${version_type}",
            &c,
        );
        assert!(result.contains("--gameDir /mc"));
        assert!(result.contains("--assetIndex 32"));
        assert!(result.contains("--username player1"));
        assert!(result.contains("--accessToken 0"));
        assert!(result.contains("--userType msa"));
        assert!(result.contains("--version 1.21.4"));
        assert!(result.contains("--versionType release"));
    }

    #[test]
    fn test_substitute_natives_dir() {
        let dir = PathBuf::from("/mc");
        let result = substitute_tokens(
            "-Djava.library.path=${natives_directory}",
            &ctx(&dir, "1.21"),
        );
        assert_eq!(result, "-Djava.library.path=/mc/natives");
    }

    #[test]
    fn test_substitute_library_directory() {
        let dir = PathBuf::from("/mc");
        let result = substitute_tokens("${library_directory}", &ctx(&dir, "1.21"));
        assert_eq!(result, "/mc/libraries");
    }

    #[test]
    fn test_substitute_untouched_unknown_token_stays() {
        let dir = PathBuf::from("/mc");
        let result = substitute_tokens("${custom_token}", &ctx(&dir, "1.21"));
        assert_eq!(result, "${custom_token}");
    }

    #[test]
    fn test_substitute_classpath() {
        let dir = PathBuf::from("/mc");
        let result = substitute_tokens("-cp ${classpath}", &ctx(&dir, "1.21"));
        // `${classpath}` and `-cp` are stripped by substitute_tokens
        assert_eq!(result, " ");
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
            "arguments": { "game": [], "jvm": [] }
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn test_build_classpath_minimal() {
        let root = minimal_root("1.21");
        let game_dir = PathBuf::from("/test/mc");
        let result = build_classpath(&game_dir, &root, "1.21").unwrap();
        assert_eq!(result, "/test/mc/versions/1.21/1.21.jar");
    }

    #[test]
    fn test_build_classpath_nonexistent_dir() {
        let root = minimal_root("1.0");
        let game_dir = PathBuf::from("/nonexistent");
        let result = build_classpath(&game_dir, &root, "1.0").unwrap();
        assert_eq!(result, "/nonexistent/versions/1.0/1.0.jar");
    }

    #[test]
    fn test_build_classpath_loader_jar_owner() {
        let root = minimal_root("fabric-loader-0.16.9-1.21");
        let game_dir = PathBuf::from("/test/mc");
        let result = build_classpath(&game_dir, &root, "1.21").unwrap();
        assert_eq!(result, "/test/mc/versions/1.21/1.21.jar");
    }

    #[test]
    fn test_maven_rel_path() {
        assert_eq!(
            maven_rel_path("net.fabricmc:fabric-loader:0.16.9"),
            Some(PathBuf::from(
                "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"
            ))
        );
        assert_eq!(
            maven_rel_path("org.lwjgl:lwjgl:3.3.3:natives-linux"),
            Some(PathBuf::from(
                "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar"
            ))
        );
        assert_eq!(maven_rel_path("only:two"), None);
        assert_eq!(maven_rel_path("a:b:c:d:e"), None);
        assert_eq!(maven_rel_path(""), None);
        assert_eq!(maven_rel_path("::"), None);
    }

    fn vanilla_json(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "mainClass": "net.minecraft.client.main.Main",
            "type": "release",
            "assetIndex": { "id": "19", "sha1": "x", "size": 1, "totalSize": 1, "url": "" },
            "assets": "19",
            "downloads": { "client": { "sha1": "y", "size": 2, "url": "" } },
            "javaVersion": { "component": "java-runtime-delta", "majorVersion": 21 },
            "libraries": [
                { "name": "com.example:vanilla:1.0", "downloads": { "artifact": { "path": "com/example/vanilla/1.0/vanilla-1.0.jar", "sha1": "v", "size": 1, "url": "" } } },
                { "name": "com.example:shared:1.0", "downloads": { "artifact": { "path": "com/example/shared/1.0/shared-1.0.jar", "sha1": "s", "size": 1, "url": "" } } },
            ],
            "arguments": { "game": ["--vanilla"], "jvm": [] }
        })
    }

    fn loader_json(id: &str, parent: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "inheritsFrom": parent,
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": [
                { "name": "net.fabricmc:fabric-loader:0.16.9" },
                { "name": "com.example:shared:2.0", "downloads": { "artifact": { "path": "com/example/shared/2.0/shared-2.0.jar", "sha1": "s2", "size": 1, "url": "" } } },
            ],
            "arguments": { "game": ["--loader"], "jvm": [] }
        })
    }

    fn seed_versions(dir: &Path, versions: &[(&str, serde_json::Value)]) {
        for (id, json) in versions {
            let version_dir = dir.join("versions").join(id);
            std::fs::create_dir_all(&version_dir).unwrap();
            std::fs::write(
                version_dir.join(format!("{id}.json")),
                serde_json::to_string(json).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn test_load_merged_root_loader() {
        let tmp = tempfile::tempdir().unwrap();
        seed_versions(
            tmp.path(),
            &[
                ("1.21", vanilla_json("1.21")),
                (
                    "fabric-loader-0.16.9-1.21",
                    loader_json("fabric-loader-0.16.9-1.21", "1.21"),
                ),
            ],
        );

        let (root, jar_id) = load_merged_root(tmp.path(), "fabric-loader-0.16.9-1.21").unwrap();
        assert_eq!(
            root.main_class,
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        assert_eq!(jar_id, "1.21");
        assert_eq!(root.asset_index.id, "19");
        let names: Vec<&str> = root.libraries.iter().map(|l| l.name.as_str()).collect();
        // Child first; same artifact at another version is a different
        // coordinate, so both are kept (loader version first on the classpath).
        assert_eq!(
            names,
            vec![
                "net.fabricmc:fabric-loader:0.16.9",
                "com.example:shared:2.0",
                "com.example:vanilla:1.0",
                "com.example:shared:1.0",
            ]
        );
        // Loader game arguments win over vanilla.
        assert_eq!(root.arguments.game.len(), 1);
    }

    #[test]
    fn test_load_merged_root_dedupes_identical_names() {
        let tmp = tempfile::tempdir().unwrap();
        let mut profile = loader_json("p", "1.21");
        // Repeat a parent entry verbatim: only one copy survives, child's first.
        profile["libraries"].as_array_mut().unwrap().push(serde_json::json!(
            { "name": "com.example:vanilla:1.0", "downloads": { "artifact": { "path": "other.jar", "sha1": "x", "size": 1, "url": "" } } }
        ));
        seed_versions(
            tmp.path(),
            &[("1.21", vanilla_json("1.21")), ("p", profile)],
        );

        let (root, _) = load_merged_root(tmp.path(), "p").unwrap();
        let vanilla: Vec<_> = root
            .libraries
            .iter()
            .filter(|l| l.name == "com.example:vanilla:1.0")
            .collect();
        assert_eq!(vanilla.len(), 1);
        assert_eq!(vanilla[0].get_rel_path().unwrap(), Path::new("other.jar"));
    }

    #[test]
    fn test_load_merged_root_vanilla_passthrough() {
        let tmp = tempfile::tempdir().unwrap();
        seed_versions(tmp.path(), &[("1.21", vanilla_json("1.21"))]);

        let (root, jar_id) = load_merged_root(tmp.path(), "1.21").unwrap();
        assert_eq!(root.main_class, "net.minecraft.client.main.Main");
        assert_eq!(jar_id, "1.21");
        assert_eq!(root.libraries.len(), 2);
    }

    #[test]
    fn test_load_merged_root_cycle_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = loader_json("a", "b");
        a["mainClass"] = serde_json::Value::String("A".to_string());
        let mut b = loader_json("b", "a");
        b["mainClass"] = serde_json::Value::String("B".to_string());
        seed_versions(tmp.path(), &[("a", a), ("b", b)]);

        let err = load_merged_root(tmp.path(), "a").unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn test_load_merged_root_missing_parent_errors() {
        let tmp = tempfile::tempdir().unwrap();
        seed_versions(tmp.path(), &[("orphan", loader_json("orphan", "nope"))]);

        let err = load_merged_root(tmp.path(), "orphan").unwrap_err();
        assert!(err.to_string().contains("Failed to read version JSON"));
    }

    #[test]
    fn test_build_classpath_includes_maven_fallback_lib() {
        let tmp = tempfile::tempdir().unwrap();
        seed_versions(
            tmp.path(),
            &[
                ("1.21", vanilla_json("1.21")),
                (
                    "fabric-loader-0.16.9-1.21",
                    loader_json("fabric-loader-0.16.9-1.21", "1.21"),
                ),
            ],
        );
        // Hash-less loader lib present on disk (no `downloads` in profile).
        let jar = tmp
            .path()
            .join("libraries/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar");
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, b"fake").unwrap();

        let (root, jar_id) = load_merged_root(tmp.path(), "fabric-loader-0.16.9-1.21").unwrap();
        let cp = build_classpath(tmp.path(), &root, &jar_id).unwrap();
        assert!(cp.contains("versions/1.21/1.21.jar"));
        assert!(cp.contains(&jar.to_string_lossy().to_string()));
        // Parent-only libs without files on disk are skipped.
        assert!(!cp.contains("vanilla-1.0.jar"));
    }

    #[test]
    fn test_validate_launchable_rejects_missing_loader() {
        use std::collections::HashMap;
        use std::sync::Mutex;

        use rusqlite::Connection;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("db/schema.sql")).unwrap();
        let (event_tx, _) = crate::events::new_event_channel();
        let state = Arc::new(AppState {
            db: Mutex::new(conn),
            event_tx,
            running: Arc::new(Mutex::new(HashMap::new())),
            active_operations: Mutex::new(HashSet::new()),
            auth_token: "test-token".into(),
        });

        let mut instance = crate::db::instances::Instance {
            id: "loader-pending".to_string(),
            name: "Pending".to_string(),
            game_version: "1.21".to_string(),
            icon: "Grass".to_string(),
            game_dir: "/tmp/pending".to_string(),
            status: crate::db::instances::InstanceStatus::Ready,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_played: None,
            playtime_seconds: 0,
            java_runtime: "java".to_string(),
            java_args: String::new(),
            modpack_source: crate::db::instances::ModpackSource::Mrpack,
            modpack_path: None,
            loader: Some("Fabric".to_string()),
            loader_version: Some("0.16.9".to_string()),
            loader_profile: None,
        };
        crate::db::instances::insert(&state.db.lock().unwrap(), &instance).unwrap();
        let err = validate_launchable(&state, "loader-pending").unwrap_err();
        assert!(err.to_string().contains("loader not installed"));

        instance.id = "loader-ready".to_string();
        instance.loader_profile = Some("fabric-loader-0.16.9-1.21".to_string());
        crate::db::instances::insert(&state.db.lock().unwrap(), &instance).unwrap();
        let ok = validate_launchable(&state, "loader-ready").unwrap();
        assert_eq!(ok.id, "loader-ready");
    }
}
