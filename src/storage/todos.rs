use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::TODOS_TABLE;
use super::error::Result;
use super::table;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: u64,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub done: bool,
}

pub fn put(db: &Database, item: &TodoItem) -> Result<()> {
    table::put_u64(db, TODOS_TABLE, item.id, item)
}

#[allow(dead_code)] // For future “remove todo” / admin tooling
pub fn get(db: &Database, id: u64) -> Result<Option<TodoItem>> {
    table::get_u64(db, TODOS_TABLE, id)
}

#[allow(dead_code)]
pub fn delete(db: &Database, id: u64) -> Result<bool> {
    table::delete_u64(db, TODOS_TABLE, id)
}

pub fn next_id(db: &Database) -> Result<u64> {
    table::next_id_u64(db, TODOS_TABLE)
}

/// Open items (`done == false`), oldest first, at most `max` rows.
pub fn list_open(db: &Database, max: usize) -> Result<Vec<TodoItem>> {
    let r = db.begin_read()?;
    let t = r.open_table(TODOS_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        let item: TodoItem = codec::decode(v.value())?;
        if !item.done {
            out.push(item);
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out.truncate(max);
    Ok(out)
}

pub fn mark_done(db: &Database, id: u64) -> Result<bool> {
    let Some(mut item) = get(db, id)? else {
        return Ok(false);
    };
    if item.done {
        return Ok(false);
    }
    item.done = true;
    put(db, &item)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    #[test]
    fn open_list_oldest_first() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let t1 = DateTime::parse_from_rfc3339("2026-04-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let t2 = DateTime::parse_from_rfc3339("2026-04-02T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        put(
            &db,
            &TodoItem {
                id: 2,
                body: "second".into(),
                created_at: t2,
                done: false,
            },
        )
        .unwrap();
        put(
            &db,
            &TodoItem {
                id: 1,
                body: "first".into(),
                created_at: t1,
                done: false,
            },
        )
        .unwrap();
        let open = list_open(&db, 10).unwrap();
        assert_eq!(open.len(), 2);
        assert_eq!(open[0].body, "first");
        assert_eq!(open[1].body, "second");
    }

    #[test]
    fn mark_done_skips_completed() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let t = Utc::now();
        put(
            &db,
            &TodoItem {
                id: 1,
                body: "x".into(),
                created_at: t,
                done: false,
            },
        )
        .unwrap();
        assert!(mark_done(&db, 1).unwrap());
        assert!(!mark_done(&db, 1).unwrap());
        assert!(list_open(&db, 10).unwrap().is_empty());
    }
}
