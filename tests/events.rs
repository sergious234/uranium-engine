use uranium_engine::events::AppEvent;

#[test]
fn test_instance_progress_serialization() {
    let event = AppEvent::InstanceProgress {
        instance_id: "uuid-123".into(),
        phase: "DownloadingAssets".into(),
        remaining: 42,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "instance:progress");
    assert_eq!(json["data"]["instance_id"], "uuid-123");
    assert_eq!(json["data"]["phase"], "DownloadingAssets");
    assert_eq!(json["data"]["remaining"], 42);
}

#[test]
fn test_instance_completed_serialization() {
    let event = AppEvent::InstanceCompleted {
        instance_id: "uuid-456".into(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "instance:completed");
    assert_eq!(json["data"]["instance_id"], "uuid-456");
    assert!(json["data"].get("phase").is_none());
}

#[test]
fn test_instance_error_serialization() {
    let event = AppEvent::InstanceError {
        instance_id: "uuid-789".into(),
        error: "Version 1.21 doesn't exist".into(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "instance:error");
    assert_eq!(json["data"]["instance_id"], "uuid-789");
    assert_eq!(json["data"]["error"], "Version 1.21 doesn't exist");
}

#[test]
fn test_game_started_serialization() {
    let event = AppEvent::GameStarted {
        instance_id: "uuid-111".into(),
        pid: 12345,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "game:started");
    assert_eq!(json["data"]["instance_id"], "uuid-111");
    assert_eq!(json["data"]["pid"], 12345);
}

#[test]
fn test_game_exited_serialization() {
    let event = AppEvent::GameExited {
        instance_id: "uuid-222".into(),
        exit_code: 0,
        playtime_seconds: 3600,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "game:exited");
    assert_eq!(json["data"]["instance_id"], "uuid-222");
    assert_eq!(json["data"]["exit_code"], 0);
    assert_eq!(json["data"]["playtime_seconds"], 3600);
}

#[test]
fn test_game_output_serialization() {
    let event = AppEvent::GameOutput {
        instance_id: "uuid-333".into(),
        stream: "stdout".into(),
        line: "[16:32:01] [Render thread/INFO]: Loaded 1234".into(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "game:output");
    assert_eq!(json["data"]["instance_id"], "uuid-333");
    assert_eq!(json["data"]["stream"], "stdout");
    assert_eq!(
        json["data"]["line"],
        "[16:32:01] [Render thread/INFO]: Loaded 1234"
    );
}

#[test]
fn test_game_output_stderr_serialization() {
    let event = AppEvent::GameOutput {
        instance_id: "uuid-444".into(),
        stream: "stderr".into(),
        line: "Error: Could not find or load main class".into(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "game:output");
    assert_eq!(json["data"]["stream"], "stderr");
    assert_eq!(
        json["data"]["line"],
        "Error: Could not find or load main class"
    );
}

#[test]
fn test_deserialize_roundtrip() {
    let original = AppEvent::InstanceProgress {
        instance_id: "roundtrip".into(),
        phase: "CheckingFiles".into(),
        remaining: 0,
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: AppEvent = serde_json::from_str(&json).unwrap();
    match deserialized {
        AppEvent::InstanceProgress {
            instance_id,
            phase,
            remaining,
        } => {
            assert_eq!(instance_id, "roundtrip");
            assert_eq!(phase, "CheckingFiles");
            assert_eq!(remaining, 0);
        }
        _ => panic!("wrong variant after roundtrip"),
    }
}
