use std::path::Path;

use rusqlite::Connection;

pub mod instances;

/// Columns added after the initial schema. Existing databases predate them,
/// so [`init`] backfills them idempotently.
const MIGRATION_COLUMNS: &[(&str, &str)] = &[
    ("modpack_source", "TEXT NOT NULL DEFAULT 'vanilla'"),
    ("modpack_path", "TEXT"),
    ("loader", "TEXT"),
    ("loader_version", "TEXT"),
    ("loader_profile", "TEXT"),
];

pub fn init(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(include_str!("schema.sql"))?;
    migrate(&conn)?;
    Ok(conn)
}

/// Adds [`MIGRATION_COLUMNS`] missing from `instances` (idempotent).
///
/// `ALTER TABLE ... ADD COLUMN` fails when the column already exists, so each
/// column is checked against `PRAGMA table_info` first. Safe to run on every
/// startup, including fresh databases where `schema.sql` already has them.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(instances)")?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    for (column, ddl) in MIGRATION_COLUMNS {
        if !existing.iter().any(|c| c == column) {
            conn.execute(
                &format!("ALTER TABLE instances ADD COLUMN {column} {ddl}"),
                [],
            )?;
        }
    }
    Ok(())
}
