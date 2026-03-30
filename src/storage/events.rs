use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::EVENTS_TABLE;
use super::error::Result;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub title: String,
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
}

/// Insert or replace an event (primary key is `event.id`).
pub fn put(db: &Database, event: &Event) -> Result<()> {
    let bytes = codec::encode(event)?;
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(EVENTS_TABLE)?;
        t.insert(event.id, bytes.as_slice())?;
    }
    w.commit()?;
    Ok(())
}

pub fn get(db: &Database, id: u64) -> Result<Option<Event>> {
    let r = db.begin_read()?;
    let t = r.open_table(EVENTS_TABLE)?;
    let Some(guard) = t.get(id)? else {
        return Ok(None);
    };
    let raw = guard.value();
    let event = codec::decode(raw)?;
    Ok(Some(event))
}

/// Returns whether a row existed and was removed.
pub fn delete(db: &Database, id: u64) -> Result<bool> {
    let w = db.begin_write()?;
    let removed = {
        let mut t = w.open_table(EVENTS_TABLE)?;
        let old = t.remove(id)?;
        old.is_some()
    };
    w.commit()?;
    Ok(removed)
}

pub fn list_all(db: &Database) -> Result<Vec<Event>> {
    let r = db.begin_read()?;
    let t = r.open_table(EVENTS_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        let event: Event = codec::decode(v.value())?;
        out.push(event);
    }
    out.sort_by_key(|e| e.id);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    fn sample(id: u64) -> Event {
        Event {
            id,
            title: "Gig".into(),
            description: Some("Doors 7pm".into()),
            start: DateTime::parse_from_rfc3339("2026-04-05T19:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end: None,
            tags: vec!["karaoke".into(), "work".into()],
        }
    }

    #[test]
    fn put_get_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let e = sample(1);
        put(&db, &e).unwrap();
        assert_eq!(get(&db, 1).unwrap(), Some(e));
        assert_eq!(get(&db, 99).unwrap(), None);
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
    fn list_all_sorted_by_id() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        put(&db, &sample(30)).unwrap();
        put(&db, &sample(10)).unwrap();
        let list = list_all(&db).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, 10);
        assert_eq!(list[1].id, 30);
    }

    #[test]
    fn replace_same_id() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let mut e = sample(1);
        put(&db, &e).unwrap();
        e.title = "Updated".into();
        put(&db, &e).unwrap();
        assert_eq!(get(&db, 1).unwrap().unwrap().title, "Updated");
    }
}
