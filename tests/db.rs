use rusqlite::Connection;

use uranium_engine::db;
use uranium_engine::db::instances::{self, Instance, InstanceStatus, ModpackSource};

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../src/db/schema.sql"))
        .unwrap();
    conn
}

fn make_instance(id: &str) -> Instance {
    Instance {
        id: id.to_string(),
        name: format!("Test {id}"),
        game_version: "1.21".to_string(),
        icon: "Diamond".to_string(),
        game_dir: format!("/tmp/test-instances/{id}"),
        status: InstanceStatus::Ready,
        created_at: "2026-06-15T10:00:00Z".to_string(),
        last_played: None,
        playtime_seconds: 0,
        java_runtime: "java".to_string(),
        java_args: "-Xmn 4Gb -Xmx 8Gb".to_string(),
        modpack_source: ModpackSource::Vanilla,
        modpack_path: None,
        loader: None,
        loader_version: None,
        loader_profile: None,
    }
}

#[test]
fn test_insert_and_get() {
    let conn = setup_db();
    let instance = make_instance("abc-123");
    instances::insert(&conn, &instance).unwrap();

    let fetched = instances::get(&conn, "abc-123")
        .unwrap()
        .expect("instance should exist");
    assert_eq!(fetched.id, "abc-123");
    assert_eq!(fetched.name, "Test abc-123");
    assert_eq!(fetched.game_version, "1.21");
    assert_eq!(fetched.icon, "Diamond");
    assert_eq!(fetched.status, InstanceStatus::Ready);
}

#[test]
fn test_get_all_empty() {
    let conn = setup_db();
    let all = instances::get_all(&conn).unwrap();
    assert!(all.is_empty());
}

#[test]
fn test_get_all_multiple() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("a")).unwrap();
    instances::insert(&conn, &make_instance("b")).unwrap();
    instances::insert(&conn, &make_instance("c")).unwrap();

    let all = instances::get_all(&conn).unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn test_get_nonexistent() {
    let conn = setup_db();
    let result = instances::get(&conn, "no-such-id").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_delete() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("to-delete")).unwrap();
    instances::delete(&conn, "to-delete").unwrap();
    let result = instances::get(&conn, "to-delete").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_rename() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("r")).unwrap();
    instances::rename(&conn, "r", "Renamed").unwrap();
    let fetched = instances::get(&conn, "r").unwrap().unwrap();
    assert_eq!(fetched.name, "Renamed");
}

#[test]
fn test_update_icon() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("i")).unwrap();
    instances::update_icon(&conn, "i", "Emerald").unwrap();
    let fetched = instances::get(&conn, "i").unwrap().unwrap();
    assert_eq!(fetched.icon, "Emerald");
}

#[test]
fn test_update_playtime() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("p")).unwrap();
    instances::update_playtime(&conn, "p", 3600).unwrap();
    let fetched = instances::get(&conn, "p").unwrap().unwrap();
    assert_eq!(fetched.playtime_seconds, 3600);
}

#[test]
fn test_update_last_played() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("lp")).unwrap();
    instances::update_last_played(&conn, "lp", "2026-06-20T18:45:00Z").unwrap();
    let fetched = instances::get(&conn, "lp").unwrap().unwrap();
    assert_eq!(fetched.last_played.as_deref(), Some("2026-06-20T18:45:00Z"));
}

#[test]
fn test_update_status() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("s")).unwrap();
    instances::update_status(&conn, "s", "downloading").unwrap();
    let fetched = instances::get(&conn, "s").unwrap().unwrap();
    assert_eq!(fetched.status, InstanceStatus::Downloading);
}

#[test]
fn test_insert_mrpack_instance_roundtrip() {
    let conn = setup_db();
    let mut instance = make_instance("mrpack-1");
    instance.modpack_source = ModpackSource::Mrpack;
    instance.modpack_path = Some("/tmp/pack.mrpack".to_string());
    instance.loader = Some("Fabric".to_string());
    instance.loader_version = Some("0.16.9".to_string());
    instance.loader_profile = Some("fabric-loader-0.16.9-1.21".to_string());
    instances::insert(&conn, &instance).unwrap();

    let fetched = instances::get(&conn, "mrpack-1").unwrap().unwrap();
    assert_eq!(fetched.modpack_source, ModpackSource::Mrpack);
    assert_eq!(fetched.modpack_path.as_deref(), Some("/tmp/pack.mrpack"));
    assert_eq!(fetched.loader.as_deref(), Some("Fabric"));
    assert_eq!(fetched.loader_version.as_deref(), Some("0.16.9"));
    assert_eq!(
        fetched.loader_profile.as_deref(),
        Some("fabric-loader-0.16.9-1.21")
    );
}

#[test]
fn test_vanilla_instance_has_null_modpack_fields() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("vanilla-1")).unwrap();

    let fetched = instances::get(&conn, "vanilla-1").unwrap().unwrap();
    assert_eq!(fetched.modpack_source, ModpackSource::Vanilla);
    assert_eq!(fetched.modpack_path, None);
    assert_eq!(fetched.loader, None);
    assert_eq!(fetched.loader_version, None);
    assert_eq!(fetched.loader_profile, None);
}

fn setup_old_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE instances (
            id              TEXT PRIMARY KEY,
            name            TEXT NOT NULL,
            game_version    TEXT NOT NULL,
            icon            TEXT DEFAULT 'Grass',
            game_dir        TEXT NOT NULL,
            status          TEXT DEFAULT 'ready',
            created_at      TEXT NOT NULL,
            last_played     TEXT,
            playtime_seconds INTEGER DEFAULT 0,
            java_runtime    TEXT NOT NULL,
            java_args       TEXT DEFAULT ''
        )",
    )
    .unwrap();
    conn
}

#[test]
fn test_migrate_adds_modpack_columns_idempotently() {
    let conn = setup_old_db();
    db::migrate(&conn).unwrap();
    db::migrate(&conn).unwrap();

    let mut stmt = conn.prepare("PRAGMA table_info(instances)").unwrap();
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for column in [
        "modpack_source",
        "modpack_path",
        "loader",
        "loader_version",
        "loader_profile",
    ] {
        assert!(columns.contains(&column.to_string()), "missing {column}");
    }

    conn.execute(
        "INSERT INTO instances (id, name, game_version, game_dir, created_at, java_runtime)
         VALUES ('old', 'Old', '1.21', '/tmp/x', '2026-01-01T00:00:00Z', 'java')",
        [],
    )
    .unwrap();
    let fetched = instances::get(&conn, "old").unwrap().unwrap();
    assert_eq!(fetched.modpack_source, ModpackSource::Vanilla);
    assert_eq!(fetched.modpack_path, None);
}
