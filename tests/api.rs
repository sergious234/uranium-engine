use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::broadcast;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use uranium_engine::db;
use uranium_engine::events::{self, AppEvent};
use uranium_engine::routes;
use uranium_engine::state::AppState;

struct TestApp {
    addr: String,
    _temp_dir: tempfile::TempDir,
    event_tx: broadcast::Sender<AppEvent>,
    state: Arc<AppState>,
}

impl TestApp {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn ws_url(&self, path: &str) -> String {
        format!("ws://{}{}?token=test-token", self.addr, path)
    }

    fn client(&self) -> reqwest::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-uranium-token", "test-token".parse().unwrap());
        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap()
    }
}

async fn setup() -> TestApp {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let conn = db::init(&db_path).unwrap();
    let (event_tx, _) = events::new_event_channel();

    let state = Arc::new(AppState {
        db: Mutex::new(conn),
        event_tx: event_tx.clone(),
        running: Arc::new(Mutex::new(HashMap::new())),
        active_operations: Mutex::new(HashSet::new()),
        auth_token: "test-token".into(),
    });

    let app = routes::router(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind test server");
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    TestApp {
        addr: format!("127.0.0.1:{}", addr.port()),
        _temp_dir: temp_dir,
        event_tx,
        state,
    }
}

fn insert_test_instance(app: &TestApp, id: &str, status: db::instances::InstanceStatus) {
    let instance = db::instances::Instance {
        id: id.into(),
        name: "Test".into(),
        game_version: "1.21".into(),
        icon: "Grass".into(),
        game_dir: app._temp_dir.path().join(id).to_string_lossy().to_string(),
        status,
        created_at: "2026-01-01T00:00:00Z".into(),
        last_played: None,
        playtime_seconds: 0,
        java_runtime: "java".into(),
        java_args: String::new(),
        modpack_source: db::instances::ModpackSource::Vanilla,
        modpack_path: None,
        loader: None,
        loader_version: None,
        loader_profile: None,
    };
    db::instances::insert(&app.state.db.lock().unwrap(), &instance).unwrap();
    std::fs::create_dir(&instance.game_dir).unwrap();
}

// ── Health ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_health() {
    let app = setup().await;
    let resp = app.client().get(app.url("/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "OK");
}

#[tokio::test]
async fn test_health_and_docs_are_public() {
    let app = setup().await;
    let client = reqwest::Client::new();
    let resp = client.get(app.url("/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "OK");
    let resp = client
        .get(app.url("/api-docs/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_http_and_websocket_reject_missing_token() {
    let app = setup().await;
    let response = reqwest::Client::new()
        .get(app.url("/instances"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(
        connect_async(format!("ws://{}/ws", app.addr))
            .await
            .is_err()
    );
}

// ── Create Instance ────────────────────────────────────────────────

#[tokio::test]
async fn test_create_instance_returns_202() {
    let app = setup().await;
    let client = app.client();
    let resp = client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "Test 1.21", "version": "1.21"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"].as_str().unwrap().to_string();
    assert!(!instance_id.is_empty());
    uuid::Uuid::parse_str(&instance_id).expect("instance_id should be a valid UUID");
}

#[tokio::test]
async fn test_create_instance_persists_to_db() {
    let app = setup().await;
    let client = app.client();
    let resp = client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "Vanilla", "version": "1.20.4", "icon": "Emerald"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    let resp = client
        .get(app.url(&format!("/instances/{instance_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let instance: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(instance["name"], "Vanilla");
    assert_eq!(instance["game_version"], "1.20.4");
    assert_eq!(instance["icon"], "Emerald");
    assert_eq!(instance["status"], "downloading");
}

// ── List Instances ─────────────────────────────────────────────────

#[tokio::test]
async fn test_list_instances_empty() {
    let app = setup().await;
    let resp = app
        .client()
        .get(app.url("/instances"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_list_instances_with_entries() {
    let app = setup().await;
    let client = app.client();
    client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "A", "version": "1.21"}))
        .send()
        .await
        .unwrap();
    client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "B", "version": "1.20.4"}))
        .send()
        .await
        .unwrap();

    let resp = client.get(app.url("/instances")).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);
}

// ── Get Single Instance ────────────────────────────────────────────

#[tokio::test]
async fn test_get_instance_not_found() {
    let app = setup().await;
    let resp = app
        .client()
        .get(app.url("/instances/no-such-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

// ── Patch Instance ─────────────────────────────────────────────────

#[tokio::test]
async fn test_patch_instance_rename() {
    let app = setup().await;
    let client = app.client();
    let create_resp = client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "Old Name", "version": "1.21"}))
        .send()
        .await
        .unwrap();
    let id = create_resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .patch(app.url(&format!("/instances/{id}")))
        .json(&serde_json::json!({"name": "New Name", "icon": "Diamond"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "New Name");
    assert_eq!(body["icon"], "Diamond");
}

#[tokio::test]
async fn test_patch_instance_not_found() {
    let app = setup().await;
    let client = app.client();
    let resp = client
        .patch(app.url("/instances/no-such-id"))
        .json(&serde_json::json!({"name": "Nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── Delete Instance ────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_instance() {
    let app = setup().await;
    let client = app.client();
    let id = "to-delete";
    insert_test_instance(&app, id, db::instances::InstanceStatus::Ready);

    let resp = client
        .delete(app.url(&format!("/instances/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(!app._temp_dir.path().join(id).exists());

    let resp = client
        .get(app.url(&format!("/instances/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_delete_rejects_installing_and_preserves_files() {
    let app = setup().await;
    insert_test_instance(
        &app,
        "installing",
        db::instances::InstanceStatus::Downloading,
    );
    let resp = app
        .client()
        .delete(app.url("/instances/installing"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert!(app._temp_dir.path().join("installing").exists());
    assert!(
        db::instances::get(&app.state.db.lock().unwrap(), "installing")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn test_delete_rejects_running_and_preserves_files() {
    let app = setup().await;
    insert_test_instance(&app, "running", db::instances::InstanceStatus::Ready);
    let (kill_tx, _) = tokio::sync::oneshot::channel();
    app.state.running.lock().unwrap().insert(
        "running".into(),
        uranium_engine::state::RunningGame {
            pid: 123,
            started_at: std::time::Instant::now(),
            kill_tx: Some(kill_tx),
        },
    );
    let resp = app
        .client()
        .delete(app.url("/instances/running"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert!(app._temp_dir.path().join("running").exists());
}

#[tokio::test]
async fn test_delete_instance_not_found() {
    let app = setup().await;
    let client = app.client();
    let resp = client
        .delete(app.url("/instances/no-such-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── Settings ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_settings_roundtrip() {
    let app = setup().await;
    let client = app.client();

    let input = serde_json::json!({
        "java_path": "/usr/lib/jvm/java-21/bin/java",
        "max_memory": "4G",
        "jvm_args": ["-XX:+UseG1GC"],
        "window_width": 1280,
        "window_height": 720,
        "show_launcher": false
    });

    let resp = client
        .put(app.url("/settings"))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client.get(app.url("/settings")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["java_path"], "/usr/lib/jvm/java-21/bin/java");
    assert_eq!(body["max_memory"], "4G");
    assert_eq!(body["window_width"], 1280);
    assert_eq!(body["window_height"], 720);
}

#[tokio::test]
async fn test_settings_returns_valid_json() {
    let app = setup().await;
    let resp = app.client().get(app.url("/settings")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Must have all expected keys (values depend on config file state)
    assert!(body.get("max_memory").is_some());
    assert!(body.get("window_width").is_some());
    assert!(body.get("window_height").is_some());
    assert!(body.get("java_path").is_some());
    assert!(body.get("jvm_args").is_some());
    assert!(body.get("show_launcher").is_some());
}

// ── Launch / Running / Terminate ───────────────────────────────────

#[tokio::test]
async fn test_running_empty() {
    let app = setup().await;
    let resp = app.client().get(app.url("/running")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["running"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_launch_not_found() {
    let app = setup().await;
    let client = app.client();
    let resp = client
        .post(app.url("/launch/no-such-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_launch_not_ready() {
    let app = setup().await;
    let client = app.client();
    let create_resp = client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "NotReady", "version": "1.21"}))
        .send()
        .await
        .unwrap();
    let id = create_resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(app.url(&format!("/launch/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_terminate_not_found() {
    let app = setup().await;
    let client = app.client();
    let resp = client
        .post(app.url("/terminate/no-such-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── WebSocket ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_websocket_connect() {
    let app = setup().await;
    let result = connect_async(app.ws_url("/ws")).await;
    assert!(result.is_ok(), "WebSocket connection should succeed");
}

#[tokio::test]
async fn test_websocket_receives_instance_progress() {
    let app = setup().await;

    let ws_url = app.ws_url("/ws");
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket connection should succeed");

    let event = AppEvent::InstanceProgress {
        instance_id: "progress-test".into(),
        phase: "DownloadingVersion".into(),
        remaining: 0,
        total: None,
    };
    app.event_tx.send(event).unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws_stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(json["event"], "instance:progress");
                    assert_eq!(json["data"]["instance_id"], "progress-test");
                    assert_eq!(json["data"]["phase"], "DownloadingVersion");
                    assert_eq!(json["data"]["remaining"], 0);
                    return;
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("WS error: {e}"),
                None => panic!("WS closed unexpectedly"),
            }
        }
    })
    .await
    .expect("Timeout waiting for WS event");
}

#[tokio::test]
async fn test_background_download_emits_error_event() {
    let app = setup().await;

    let ws_url = app.ws_url("/ws");
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket connection should succeed");

    let client = app.client();
    client
        .post(app.url("/instances"))
        .json(&serde_json::json!({"name": "BSOD", "version": "999.999.999"}))
        .send()
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws_stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if json["event"] == "instance:error" {
                        assert!(!json["data"]["error"].as_str().unwrap().is_empty());
                        return;
                    }
                    // Also accept progress events
                    if json["event"] == "instance:progress" {
                        continue;
                    }
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("WS error: {e}"),
                None => panic!("WS closed before receiving error event"),
            }
        }
    })
    .await
    .expect("Timeout: did not receive instance:error event");
}
