use rusqlite::{params, Connection, OptionalExtension as _};

use super::{meta_get, meta_set, DbError, KEY_ACTIVE_CONFIG};

pub const DEFAULT_CONTENT: &str = "{}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    pub id: i64,
    pub name: String,
    pub content: String,
    pub last_used_with_version: Option<String>,
    pub created_at: String,
    pub modified_at: String,
}

/// Fields a PATCH/PUT may change. `None` means "absent from the body", which
/// leaves the stored value alone; `Some(None)` explicitly nulls the column.
#[derive(Debug, Default, Clone)]
pub struct ConfigChanges {
    pub name: Option<String>,
    pub content: Option<String>,
    pub last_used_with_version: Option<Option<String>>,
}

const SELECT_COLUMNS: &str = "id, name, content, last_used_with_version, created_at, modified_at";

fn row_from(record: &rusqlite::Row<'_>) -> rusqlite::Result<ConfigRow> {
    Ok(ConfigRow {
        id: record.get(0)?,
        name: record.get(1)?,
        content: record.get(2)?,
        last_used_with_version: record.get(3)?,
        created_at: record.get(4)?,
        modified_at: record.get(5)?,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<ConfigRow>, DbError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM configs ORDER BY id ASC"
    ))?;
    let rows = statement.query_map([], row_from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<ConfigRow>, DbError> {
    let row = conn
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM configs WHERE id = ?1"),
            params![id],
            row_from,
        )
        .optional()?;
    Ok(row)
}

pub fn exists(conn: &Connection, id: i64) -> Result<bool, DbError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM configs WHERE id = ?1",
            params![id],
            |record| record.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

#[derive(Debug, Clone)]
pub struct NewConfig<'a> {
    pub name: &'a str,
    pub content: Option<&'a str>,
    pub last_used_with_version: Option<&'a str>,
    pub timestamp: &'a str,
}

pub fn create(conn: &Connection, new: NewConfig<'_>) -> Result<ConfigRow, DbError> {
    let content = new.content.unwrap_or(DEFAULT_CONTENT);
    conn.execute(
        "INSERT INTO configs (name, content, last_used_with_version, created_at, modified_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![new.name, content, new.last_used_with_version, new.timestamp],
    )?;
    let id = conn.last_insert_rowid();

    Ok(ConfigRow {
        id,
        name: new.name.to_owned(),
        content: content.to_owned(),
        last_used_with_version: new.last_used_with_version.map(str::to_owned),
        created_at: new.timestamp.to_owned(),
        modified_at: new.timestamp.to_owned(),
    })
}

/// Read-modify-write in one transaction, so `content` and `modified_at` always
/// move together (last write wins, like upstream).
pub fn update(
    conn: &mut Connection,
    id: i64,
    changes: &ConfigChanges,
    timestamp: &str,
) -> Result<Option<ConfigRow>, DbError> {
    let tx = conn.transaction()?;
    let Some(current) = get(&tx, id)? else {
        return Ok(None);
    };

    let name = changes.name.clone().unwrap_or(current.name);
    let content = changes.content.clone().unwrap_or(current.content);
    let last_used_with_version = match changes.last_used_with_version.clone() {
        Some(value) => value,
        None => current.last_used_with_version,
    };

    tx.execute(
        "UPDATE configs
         SET name = ?1, content = ?2, last_used_with_version = ?3, modified_at = ?4
         WHERE id = ?5",
        params![name, content, last_used_with_version, timestamp, id],
    )?;
    tx.commit()?;

    Ok(Some(ConfigRow {
        id,
        name,
        content,
        last_used_with_version,
        created_at: current.created_at,
        modified_at: timestamp.to_owned(),
    }))
}

/// Returns whether a row was deleted. `active_config` is cleared the way
/// upstream's `on_delete=models.SET_NULL` does it.
pub fn delete(conn: &mut Connection, id: i64) -> Result<bool, DbError> {
    let tx = conn.transaction()?;
    let removed = tx.execute("DELETE FROM configs WHERE id = ?1", params![id])?;
    if removed > 0 {
        let id_text = id.to_string();
        if meta_get(&tx, KEY_ACTIVE_CONFIG)?.as_deref() == Some(id_text.as_str()) {
            meta_set(&tx, KEY_ACTIVE_CONFIG, None)?;
        }
    }
    tx.commit()?;
    Ok(removed > 0)
}

#[cfg(test)]
mod tests {
    use super::super::Db;
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("db")
    }

    #[test]
    fn create_applies_defaults_and_shares_timestamps() {
        let db = db();
        let conn = db.lock().expect("lock");
        let row = create(
            &conn,
            NewConfig {
                name: "New config",
                content: None,
                last_used_with_version: None,
                timestamp: "2026-09-12T09:41:07.123456Z",
            },
        )
        .expect("create");

        assert_eq!(row.id, 1);
        assert_eq!(row.content, "{}");
        assert_eq!(row.last_used_with_version, None);
        assert_eq!(row.created_at, row.modified_at);
        assert_eq!(get(&conn, 1).expect("get").as_ref(), Some(&row));
    }

    #[test]
    fn ids_are_never_reused() {
        let db = db();
        let mut conn = db.lock().expect("lock");
        let first = create(
            &conn,
            NewConfig {
                name: "a",
                content: None,
                last_used_with_version: None,
                timestamp: "2026-09-12T09:41:07.000000Z",
            },
        )
        .expect("create");
        assert!(delete(&mut conn, first.id).expect("delete"));

        let second = create(
            &conn,
            NewConfig {
                name: "b",
                content: None,
                last_used_with_version: None,
                timestamp: "2026-09-12T09:41:08.000000Z",
            },
        )
        .expect("create");
        assert_eq!(second.id, 2);
        assert_eq!(get(&conn, 1).expect("get"), None);
    }

    #[test]
    fn update_only_touches_provided_fields() {
        let db = db();
        let mut conn = db.lock().expect("lock");
        let created = create(
            &conn,
            NewConfig {
                name: "keep me",
                content: Some("original"),
                last_used_with_version: Some("1.0.235"),
                timestamp: "2026-09-12T09:41:07.000000Z",
            },
        )
        .expect("create");

        let updated = update(
            &mut conn,
            created.id,
            &ConfigChanges {
                content: Some("new content".to_owned()),
                ..Default::default()
            },
            "2026-09-12T09:58:22.000000Z",
        )
        .expect("update")
        .expect("row exists");

        assert_eq!(updated.name, "keep me");
        assert_eq!(updated.content, "new content");
        assert_eq!(updated.last_used_with_version.as_deref(), Some("1.0.235"));
        assert_eq!(updated.created_at, created.created_at);
        assert_eq!(updated.modified_at, "2026-09-12T09:58:22.000000Z");

        let nulled = update(
            &mut conn,
            created.id,
            &ConfigChanges {
                last_used_with_version: Some(None),
                ..Default::default()
            },
            "2026-09-12T10:00:00.000000Z",
        )
        .expect("update")
        .expect("row exists");
        assert_eq!(nulled.last_used_with_version, None);
        assert_eq!(nulled.content, "new content");
    }

    #[test]
    fn update_of_missing_row_reports_none() {
        let db = db();
        let mut conn = db.lock().expect("lock");
        assert!(update(
            &mut conn,
            42,
            &ConfigChanges {
                content: Some("x".to_owned()),
                ..Default::default()
            },
            "2026-09-12T09:41:07.000000Z",
        )
        .expect("update")
        .is_none());
    }

    #[test]
    fn delete_clears_active_config() {
        let db = db();
        let mut conn = db.lock().expect("lock");
        let row = create(
            &conn,
            NewConfig {
                name: "active",
                content: None,
                last_used_with_version: None,
                timestamp: "2026-09-12T09:41:07.000000Z",
            },
        )
        .expect("create");
        meta_set(&conn, KEY_ACTIVE_CONFIG, Some(&row.id.to_string())).expect("meta");

        assert!(delete(&mut conn, row.id).expect("delete"));
        assert_eq!(meta_get(&conn, KEY_ACTIVE_CONFIG).expect("meta"), None);
        assert!(!delete(&mut conn, row.id).expect("delete"));
    }

    #[test]
    fn list_is_ordered_by_id() {
        let db = db();
        let conn = db.lock().expect("lock");
        for name in ["a", "b", "c"] {
            create(
                &conn,
                NewConfig {
                    name,
                    content: None,
                    last_used_with_version: None,
                    timestamp: "2026-09-12T09:41:07.000000Z",
                },
            )
            .expect("create");
        }
        let rows = list(&conn).expect("list");
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(exists(&conn, 2).expect("exists"));
        assert!(!exists(&conn, 9).expect("exists"));
    }
}
