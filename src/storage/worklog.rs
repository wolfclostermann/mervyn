use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::WORKLOG_TABLE;
use super::error::Result;
use super::table;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklogEntry {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub body: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
}

pub fn put(db: &Database, entry: &WorklogEntry) -> Result<()> {
    table::put_u64(db, WORKLOG_TABLE, entry.id, entry)
}

pub fn get(db: &Database, id: u64) -> Result<Option<WorklogEntry>> {
    table::get_u64(db, WORKLOG_TABLE, id)
}

pub fn delete(db: &Database, id: u64) -> Result<bool> {
    table::delete_u64(db, WORKLOG_TABLE, id)
}

pub fn list_all(db: &Database) -> Result<Vec<WorklogEntry>> {
    table::list_all_u64(db, WORKLOG_TABLE, |e: &WorklogEntry| e.id)
}

/// Next numeric id (max key + 1).
pub fn next_id(db: &Database) -> Result<u64> {
    table::next_id_u64(db, WORKLOG_TABLE)
}

/// Entries with `timestamp >= since`, newest first, at most `max` rows.
pub fn recent_since(db: &Database, since: DateTime<Utc>, max: usize) -> Result<Vec<WorklogEntry>> {
    let r = db.begin_read()?;
    let t = r.open_table(WORKLOG_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        let entry: WorklogEntry = codec::decode(v.value())?;
        if entry.timestamp >= since {
            out.push(entry);
        }
    }
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    out.truncate(max);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    fn sample(id: u64) -> WorklogEntry {
        WorklogEntry {
            id,
            timestamp: DateTime::parse_from_rfc3339("2026-03-30T18:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            body: "Shipped storage layer".into(),
            tags: vec!["mervyn".into()],
            project: Some("mervyn".into()),
        }
    }

    #[test]
    fn put_get_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let e = sample(42);
        put(&db, &e).unwrap();
        assert_eq!(get(&db, 42).unwrap(), Some(e));
    }

    #[test]
    fn list_all_sorted() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        put(&db, &sample(300)).unwrap();
        put(&db, &sample(100)).unwrap();
        let list = list_all(&db).unwrap();
        assert_eq!(list[0].id, 100);
        assert_eq!(list[1].id, 300);
    }

    #[test]
    fn delete_removes() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        put(&db, &sample(1)).unwrap();
        assert!(delete(&db, 1).unwrap());
        assert!(!delete(&db, 1).unwrap());
        assert_eq!(get(&db, 1).unwrap(), None);
    }

    #[test]
    fn recent_since_orders_newest_first() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let since = DateTime::parse_from_rfc3339("2026-03-29T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut old = sample(1);
        old.timestamp = DateTime::parse_from_rfc3339("2026-03-28T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut new = sample(2);
        new.timestamp = DateTime::parse_from_rfc3339("2026-03-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        put(&db, &old).unwrap();
        put(&db, &new).unwrap();

        let list = recent_since(&db, since, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 2);
    }
}
