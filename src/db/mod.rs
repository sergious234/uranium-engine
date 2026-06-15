use std::path::Path;

use rusqlite::Connection;

pub mod instances;

pub fn init(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(include_str!("schema.sql"))?;
    Ok(conn)
}
