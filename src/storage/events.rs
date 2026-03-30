use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::EVENTS_TABLE;
use super::error::Result;
use super::table;

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
    table::put_u64(db, EVENTS_TABLE, event.id, event)
}

pub fn get(db: &Database, id: u64) -> Result<Option<Event>> {
    table::get_u64(db, EVENTS_TABLE, id)
}

/// Returns whether a row existed and was removed.
pub fn delete(db: &Database, id: u64) -> Result<bool> {
    table::delete_u64(db, EVENTS_TABLE, id)
}

pub fn list_all(db: &Database) -> Result<Vec<Event>> {
    table::list_all_u64(db, EVENTS_TABLE, |e: &Event| e.id)
}

/// Events whose `start` falls in `[from, until]`, ordered by `start`, at most `max` rows.
/// Full table scan (no secondary index); `max` bounds work for briefings and Claude context.
/// Next numeric id (max key + 1). For app-created rows; vault sync uses hashed ids.
pub fn next_id(db: &Database) -> Result<u64> {
    table::next_id_u64(db, EVENTS_TABLE)
}

pub fn upcoming_within(
    db: &Database,
    from: DateTime<Utc>,
    until: DateTime<Utc>,
    max: usize,
) -> Result<Vec<Event>> {
    let r = db.begin_read()?;
    let t = r.open_table(EVENTS_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        let event: Event = codec::decode(v.value())?;
        if event.start >= from && event.start <= until {
            out.push(event);
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start));
    out.truncate(max);
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

    #[test]
    fn upcoming_within_filters_and_caps() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let window_start = DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let window_end = DateTime::parse_from_rfc3339("2026-04-10T23:59:59Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut inside = sample(1);
        inside.start = DateTime::parse_from_rfc3339("2026-04-05T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut too_early = sample(2);
        too_early.start = DateTime::parse_from_rfc3339("2026-03-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut too_late = sample(3);
        too_late.start = DateTime::parse_from_rfc3339("2026-05-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        put(&db, &too_early).unwrap();
        put(&db, &inside).unwrap();
        put(&db, &too_late).unwrap();

        let list = upcoming_within(&db, window_start, window_end, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 1);
    }
}
