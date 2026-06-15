use rusqlite::Connection;

use uranium_engine::db::instances::{self, Instance};

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
        status: "ready".to_string(),
        created_at: "2026-06-15T10:00:00Z".to_string(),
        last_played: None,
        playtime_seconds: 0,
        java_runtime: "java".to_string(),
        java_args: "-Xmn 4Gb -Xmx 8Gb".to_string()
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
    assert_eq!(fetched.status, "ready");
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
    assert_eq!(
        fetched.last_played.as_deref(),
        Some("2026-06-20T18:45:00Z")
    );
}

#[test]
fn test_update_status() {
    let conn = setup_db();
    instances::insert(&conn, &make_instance("s")).unwrap();
    instances::update_status(&conn, "s", "downloading").unwrap();
    let fetched = instances::get(&conn, "s").unwrap().unwrap();
    assert_eq!(fetched.status, "downloading");
}
