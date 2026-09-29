use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use uranium_engine::db;
use uranium_engine::events;

struct TestApp {
    addr: String,
    _temp_dir: tempfile::TempDir,
    pack_dir: PathBuf,
}

impl TestApp {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

async fn setup() -> TestApp {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let conn = db::init(&db_path).unwrap();
    let (event_tx, _) = events::new_event_channel();

    let state = Arc::new(uranium_engine::state::AppState {
        db: Mutex::new(conn),
        event_tx,
        running: Arc::new(Mutex::new(HashMap::new())),
        active_operations: Mutex::new(HashSet::new()),
    });

    let app = uranium_engine::routes::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind test server");
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let pack_dir = temp_dir.path().join("packs");
    std::fs::create_dir(&pack_dir).unwrap();

    TestApp {
        addr: format!("127.0.0.1:{}", addr.port()),
        _temp_dir: temp_dir,
        pack_dir,
    }
}

fn index_json(deps: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.0.0",
        "name": "Test Pack",
        "dependencies": deps,
        "files": [],
    })
}

/// Writes a minimal `.mrpack` (manifest only) into `dir`.
fn write_mrpack(dir: &Path, name: &str, index: &serde_json::Value) -> PathBuf {
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(
        "modrinth.index.json",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(serde_json::to_string(index).unwrap().as_bytes())
        .unwrap();
    zip.finish().unwrap();
    path
}

#[tokio::test]
async fn test_mrpack_missing_file_returns_400() {
    let app = setup().await;
    let resp = reqwest::Client::new()
        .post(app.url("/instances/mrpack"))
        .json(&serde_json::json!({
            "name": "Missing",
            "mrpack_path": app.pack_dir.join("nope.mrpack").to_string_lossy(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_mrpack_wrong_extension_returns_400() {
    let app = setup().await;
    let path = app.pack_dir.join("pack.txt");
    std::fs::write(&path, "not a pack").unwrap();
    let resp = reqwest::Client::new()
        .post(app.url("/instances/mrpack"))
        .json(&serde_json::json!({
            "name": "WrongExt",
            "mrpack_path": path.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_mrpack_invalid_zip_returns_400() {
    let app = setup().await;
    let path = app.pack_dir.join("bogus.mrpack");
    std::fs::write(&path, "this is not a zip").unwrap();
    let resp = reqwest::Client::new()
        .post(app.url("/instances/mrpack"))
        .json(&serde_json::json!({
            "name": "Bogus",
            "mrpack_path": path.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_mrpack_missing_version_returns_400() {
    let app = setup().await;
    let path = write_mrpack(
        &app.pack_dir,
        "nodeps.mrpack",
        &index_json(serde_json::json!({})),
    );
    let resp = reqwest::Client::new()
        .post(app.url("/instances/mrpack"))
        .json(&serde_json::json!({
            "name": "NoDeps",
            "mrpack_path": path.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("version"));
}

#[tokio::test]
async fn test_mrpack_accepts_and_persists_row() {
    let app = setup().await;
    let path = write_mrpack(
        &app.pack_dir,
        "pack.mrpack",
        &index_json(serde_json::json!({
            "minecraft": "1.21",
            "fabric-loader": "0.16.9",
        })),
    );

    // Bogus override so the background vanilla install fails fast at version
    // lookup (online and offline) instead of downloading a real version.
    let client = reqwest::Client::new();
    let resp = client
        .post(app.url("/instances/mrpack"))
        .json(&serde_json::json!({
            "name": "Pack Instance",
            "mrpack_path": path.to_string_lossy(),
            "version_override": "0.0.0-nonexistent",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"].as_str().unwrap().to_string();
    uuid::Uuid::parse_str(&instance_id).expect("instance_id should be a valid UUID");

    let resp = client
        .get(app.url(&format!("/instances/{instance_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let instance: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(instance["name"], "Pack Instance");
    assert_eq!(instance["game_version"], "0.0.0-nonexistent");
    assert_eq!(instance["modpack_source"], "mrpack");
    assert_eq!(
        instance["modpack_path"].as_str().unwrap(),
        path.to_string_lossy()
    );
    assert_eq!(instance["loader"], "Fabric");
    assert_eq!(instance["loader_version"], "0.16.9");
    assert_eq!(instance["loader_profile"], serde_json::Value::Null);
    // The background task may already have marked it "error" (bogus version
    // fails version lookup); both states prove the row was persisted.
    assert!(["downloading", "error"].contains(&instance["status"].as_str().unwrap()));
}

#[tokio::test]
async fn test_openapi_contains_mrpack_routes() {
    let app = setup().await;
    let spec: serde_json::Value = reqwest::Client::new()
        .get(app.url("/api-docs/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(spec["paths"].get("/instances/mrpack").is_some());
    assert!(spec["paths"].get("/instances/{id}/loader").is_some());
    assert!(
        spec["components"]["schemas"]["Instance"]["properties"]
            .get("loader_profile")
            .is_some()
    );
}

#[tokio::test]
async fn test_mrpack_forge_returns_400() {
    let app = setup().await;
    let path = write_mrpack(
        &app.pack_dir,
        "forge.mrpack",
        &index_json(serde_json::json!({
            "minecraft": "1.21",
            "forge": "47.0.0",
        })),
    );
    let resp = reqwest::Client::new()
        .post(app.url("/instances/mrpack"))
        .json(&serde_json::json!({
            "name": "Forge Pack",
            "mrpack_path": path.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("not supported"));
}

#[tokio::test]
async fn test_loader_install_unknown_instance_returns_404() {
    let app = setup().await;
    let resp = reqwest::Client::new()
        .post(app.url("/instances/no-such-id/loader"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_loader_install_not_ready_returns_400() {
    let app = setup().await;
    let client = reqwest::Client::new();
    let create_resp = client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "Vanilla", "version": "1.21"}))
        .send()
        .await
        .unwrap();
    let id = create_resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(app.url(&format!("/instances/{id}/loader")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_loader_install_mrpack_downloading_returns_400() {
    let app = setup().await;
    let path = write_mrpack(
        &app.pack_dir,
        "pending.mrpack",
        &index_json(serde_json::json!({
            "minecraft": "1.21",
            "fabric-loader": "0.16.9",
        })),
    );
    let client = reqwest::Client::new();
    let create_resp = client
        .post(app.url("/instances/mrpack"))
        .json(&serde_json::json!({
            "name": "Pending",
            "mrpack_path": path.to_string_lossy(),
            "version_override": "0.0.0-nonexistent",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 202);
    let id = create_resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Still downloading (or already errored on the bogus version) — either
    // way not ready, so the repair endpoint must refuse.
    let resp = client
        .post(app.url(&format!("/instances/{id}/loader")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
