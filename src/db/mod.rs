pub mod configs;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension as _};
use thiserror::Error;

pub const KEY_SCHEMA_VERSION: &str = "schema_version";
pub const KEY_ACTIVE_CONFIG: &str = "active_config";
pub const KEY_CUSTOM_GATEWAY: &str = "custom_connection_gateway";
pub const KEY_CUSTOM_GATEWAY_TOKEN: &str = "custom_connection_gateway_token";

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("database lock is poisoned")]
    Poisoned,

    #[error("database migration {version} failed: {source}")]
    Migration {
        version: i64,
        #[source]
        source: rusqlite::Error,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Forward-only, embedded migrations. Never edit an applied entry; append.
const MIGRATIONS: &[(i64, &str)] = &[(
    1,
    r#"
    CREATE TABLE configs (
        id                     INTEGER PRIMARY KEY AUTOINCREMENT,
        name                   TEXT NOT NULL,
        content                TEXT NOT NULL DEFAULT '{}',
        last_used_with_version TEXT,
        created_at             TEXT NOT NULL,
        modified_at            TEXT NOT NULL
    );
    "#,
)];

#[derive(Debug, Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Self::with_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(conn: Connection) -> Result<Self, DbError> {
        let mut conn = conn;
        configure(&conn)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// The only way to reach the connection. Never hold the guard across an
    /// `await`: a poisoned lock would take the whole process down with it.
    pub fn lock(&self) -> Result<MutexGuard<'_, Connection>, DbError> {
        match self.conn.lock() {
            Ok(guard) => Ok(guard),
            Err(_) => Err(DbError::Poisoned),
        }
    }
}

fn configure(conn: &Connection) -> Result<(), DbError> {
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // In-memory databases silently keep their `memory` journal mode.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
    )?;

    let current: i64 = meta_get(&tx, KEY_SCHEMA_VERSION)?
        .map(|v| v.parse().unwrap_or(0))
        .unwrap_or(0);

    for &(version, sql) in MIGRATIONS {
        if version > current {
            tx.execute_batch(sql)
                .map_err(|source| DbError::Migration { version, source })?;
            meta_set(&tx, KEY_SCHEMA_VERSION, Some(&version.to_string()))?;
        }
    }

    tx.commit()?;
    Ok(())
}

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>, DbError> {
    let value = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(value)
}

/// `None` removes the key, which is how a null value is represented (the
/// column is `NOT NULL`).
pub fn meta_set(conn: &Connection, key: &str, value: Option<&str>) -> Result<(), DbError> {
    match value {
        Some(value) => {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        None => {
            conn.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_in_memory_and_migrates() {
        let db = Db::open_in_memory().expect("in-memory db");
        let conn = db.lock().expect("lock");
        assert_eq!(
            meta_get(&conn, KEY_SCHEMA_VERSION)
                .expect("meta")
                .as_deref(),
            Some("1")
        );
        let count: i64 = conn
            .query_row("SELECT count(*) FROM configs", [], |r| r.get(0))
            .expect("configs table");
        assert_eq!(count, 0);
    }

    #[test]
    fn migration_is_idempotent() {
        let db = Db::open_in_memory().expect("first");
        let mut conn = db.lock().expect("lock");
        migrate(&mut conn).expect("second migrate");
        assert_eq!(
            meta_get(&conn, KEY_SCHEMA_VERSION)
                .expect("meta")
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn meta_set_roundtrips_and_removes() {
        let db = Db::open_in_memory().expect("db");
        let conn = db.lock().expect("lock");
        meta_set(&conn, "k", Some("v")).expect("set");
        assert_eq!(meta_get(&conn, "k").expect("get").as_deref(), Some("v"));
        meta_set(&conn, "k", None).expect("unset");
        assert_eq!(meta_get(&conn, "k").expect("get"), None);
    }
}
