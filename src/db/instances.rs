use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// A Minecraft instance stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// UUIDv4 unique identifier.
    pub id: String,
    /// Human-readable display name (e.g. `"My 1.21"`).
    pub name: String,
    /// Minecraft version string (e.g. `"1.21"`, `"1.20.4"`).
    pub game_version: String,
    /// Launcher profile icon name (e.g. `"Diamond"`, `"Grass"`).
    pub icon: String,
    /// Absolute path to the instance's game directory
    /// (`{data_dir}/instances/{id}`).
    pub game_dir: String,
    /// Current status: `"downloading"`, `"ready"`, or `"error"`.
    pub status: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 timestamp of the last time the game was played, or `None`.
    pub last_played: Option<String>,
    /// Total accumulated playtime in seconds.
    pub playtime_seconds: i64,
    /// Java runtime path.
    pub java_runtime: String,
    /// Java args.
    pub java_args: String,
}

/// Insert a new instance into the database.
pub fn insert(conn: &Connection, instance: &Instance) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO instances (id, name, game_version, icon, game_dir, status, created_at, java_runtime, java_args)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            instance.id,
            instance.name,
            instance.game_version,
            instance.icon,
            instance.game_dir,
            instance.status,
            instance.created_at,
            instance.java_runtime,
            instance.java_args,
        ],
    )?;
    Ok(())
}

/// Retrieve all instances, ordered by creation date descending.
pub fn get_all(conn: &Connection) -> Result<Vec<Instance>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, game_version, icon, game_dir, status, created_at, last_played, playtime_seconds, java_runtime, java_args
         FROM instances ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Instance {
            id: row.get(0)?,
            name: row.get(1)?,
            game_version: row.get(2)?,
            icon: row.get(3)?,
            game_dir: row.get(4)?,
            status: row.get(5)?,
            created_at: row.get(6)?,
            last_played: row.get(7)?,
            playtime_seconds: row.get(8)?,
            java_runtime: row.get(9)?,
            java_args: row.get(10)?
        })
    })?;
    let mut instances = Vec::new();
    for row in rows {
        instances.push(row?);
    }
    Ok(instances)
}

/// Retrieve a single instance by ID. Returns `None` if not found.
pub fn get(conn: &Connection, id: &str) -> Result<Option<Instance>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, game_version, icon, game_dir, status, created_at, last_played, playtime_seconds, java_runtime, java_args
         FROM instances WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(Instance {
            id: row.get(0)?,
            name: row.get(1)?,
            game_version: row.get(2)?,
            icon: row.get(3)?,
            game_dir: row.get(4)?,
            status: row.get(5)?,
            created_at: row.get(6)?,
            last_played: row.get(7)?,
            playtime_seconds: row.get(8)?,
            java_runtime: row.get(9)?,
            java_args: row.get(10)?
        })
    })?;
    match rows.next() {
        Some(Ok(instance)) => Ok(Some(instance)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Delete an instance by ID.
pub fn delete(conn: &Connection, id: &str) -> Result<(), AppError> {
    conn.execute("DELETE FROM instances WHERE id = ?1", params![id])?;
    Ok(())
}

/// Rename an instance.
pub fn rename(conn: &Connection, id: &str, new_name: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE instances SET name = ?1 WHERE id = ?2",
        params![new_name, id],
    )?;
    Ok(())
}

/// Update an instance's icon.
pub fn update_icon(conn: &Connection, id: &str, new_icon: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE instances SET icon = ?1 WHERE id = ?2",
        params![new_icon, id],
    )?;
    Ok(())
}

/// Update an instance's runtime.
pub fn update_runtime(conn: &Connection, id: &str, new_runtime: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE instances SET java_runtime = ?1 WHERE id = ?2",
        params![new_runtime, id],
    )?;
    Ok(())
}

/// Update an instance's args.
pub fn update_args(conn: &Connection, id: &str, new_args: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE instances SET java_args = ?1 WHERE id = ?2",
        params![new_args, id],
    )?;
    Ok(())
}

/// Update an instance's status.
pub fn update_status(conn: &Connection, id: &str, status: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE instances SET status = ?1 WHERE id = ?2",
        params![status, id],
    )?;
    Ok(())
}

/// Set accumulated playtime in seconds.
pub fn update_playtime(conn: &Connection, id: &str, seconds: i64) -> Result<(), AppError> {
    conn.execute(
        "UPDATE instances SET playtime_seconds = ?1 WHERE id = ?2",
        params![seconds, id],
    )?;
    Ok(())
}

/// Set the last-played timestamp.
pub fn update_last_played(conn: &Connection, id: &str, timestamp: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE instances SET last_played = ?1 WHERE id = ?2",
        params![timestamp, id],
    )?;
    Ok(())
}
