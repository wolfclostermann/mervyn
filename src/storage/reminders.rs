use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::REMINDERS_TABLE;
use super::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: u64,
    pub body: String,
    pub due: DateTime<Utc>,
    pub recurrence: Option<Recurrence>,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recurrence {
    Daily,
    Weekly,
    Monthly,
    Custom(String),
}

pub fn put(db: &Database, reminder: &Reminder) -> Result<()> {
    let bytes = codec::encode(reminder)?;
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(REMINDERS_TABLE)?;
        t.insert(reminder.id, bytes.as_slice())?;
    }
    w.commit()?;
    Ok(())
}

pub fn get(db: &Database, id: u64) -> Result<Option<Reminder>> {
    let r = db.begin_read()?;
    let t = r.open_table(REMINDERS_TABLE)?;
    let Some(guard) = t.get(id)? else {
        return Ok(None);
    };
    Ok(Some(codec::decode(guard.value())?))
}

pub fn delete(db: &Database, id: u64) -> Result<bool> {
    let w = db.begin_write()?;
    let removed = {
        let mut t = w.open_table(REMINDERS_TABLE)?;
        let old = t.remove(id)?;
        old.is_some()
    };
    w.commit()?;
    Ok(removed)
}

pub fn list_all(db: &Database) -> Result<Vec<Reminder>> {
    let r = db.begin_read()?;
    let t = r.open_table(REMINDERS_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        let reminder: Reminder = codec::decode(v.value())?;
        out.push(reminder);
    }
    out.sort_by_key(|r| r.id);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    fn sample(id: u64) -> Reminder {
        Reminder {
            id,
            body: "Call accountant".into(),
            due: DateTime::parse_from_rfc3339("2026-04-05T09:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            recurrence: Some(Recurrence::Monthly),
            done: false,
        }
    }

    #[test]
    fn put_get_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let r = sample(1);
        put(&db, &r).unwrap();
        assert_eq!(get(&db, 1).unwrap(), Some(r));
    }

    #[test]
    fn delete_and_list() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        put(&db, &sample(1)).unwrap();
        put(&db, &sample(2)).unwrap();
        assert_eq!(list_all(&db).unwrap().len(), 2);
        assert!(delete(&db, 1).unwrap());
        assert_eq!(list_all(&db).unwrap().len(), 1);
        assert_eq!(get(&db, 2).unwrap().unwrap().id, 2);
    }
}
