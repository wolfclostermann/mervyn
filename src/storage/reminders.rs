use chrono::{DateTime, Months, Utc};
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::REMINDERS_TABLE;
use super::error::Result;
use super::table;

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

/// Next occurrence after the current `due` when a recurring reminder fires. `None` ⇒ mark done.
pub fn next_due_after_fire(reminder: &Reminder) -> Option<DateTime<Utc>> {
    let from = reminder.due;
    match &reminder.recurrence {
        None => None,
        Some(Recurrence::Daily) => Some(from + chrono::Duration::days(1)),
        Some(Recurrence::Weekly) => Some(from + chrono::Duration::weeks(1)),
        Some(Recurrence::Monthly) => from.checked_add_months(Months::new(1)),
        Some(Recurrence::Custom(s)) => {
            let sl = s.to_lowercase();
            if sl.contains("year") {
                from.checked_add_months(Months::new(12))
            } else if sl.contains("month") {
                from.checked_add_months(Months::new(1))
            } else if sl.contains("week") {
                Some(from + chrono::Duration::weeks(1))
            } else if sl.contains("day") {
                Some(from + chrono::Duration::days(1))
            } else {
                None
            }
        }
    }
}

pub fn put(db: &Database, reminder: &Reminder) -> Result<()> {
    table::put_u64(db, REMINDERS_TABLE, reminder.id, reminder)
}

pub fn get(db: &Database, id: u64) -> Result<Option<Reminder>> {
    table::get_u64(db, REMINDERS_TABLE, id)
}

pub fn delete(db: &Database, id: u64) -> Result<bool> {
    table::delete_u64(db, REMINDERS_TABLE, id)
}

pub fn list_all(db: &Database) -> Result<Vec<Reminder>> {
    table::list_all_u64(db, REMINDERS_TABLE, |r: &Reminder| r.id)
}

/// Next numeric id (max key + 1).
pub fn next_id(db: &Database) -> Result<u64> {
    table::next_id_u64(db, REMINDERS_TABLE)
}

/// Pending reminders (`done == false`) with `due <= at`, ordered by `due`, at most `max` rows.
/// Used by the reminder sweep job.
pub fn pending_due_by(db: &Database, at: DateTime<Utc>, max: usize) -> Result<Vec<Reminder>> {
    let r = db.begin_read()?;
    let t = r.open_table(REMINDERS_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        let reminder: Reminder = codec::decode(v.value())?;
        if !reminder.done && reminder.due <= at {
            out.push(reminder);
        }
    }
    out.sort_by(|a, b| a.due.cmp(&b.due));
    out.truncate(max);
    Ok(out)
}

/// Pending reminders with `due <= until` (includes overdue when `until` is now or later),
/// ordered by `due`, at most `max` rows. For briefings: pass end of horizon (e.g. today + 7d).
pub fn pending_due_within(
    db: &Database,
    until: DateTime<Utc>,
    max: usize,
) -> Result<Vec<Reminder>> {
    let r = db.begin_read()?;
    let t = r.open_table(REMINDERS_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        let reminder: Reminder = codec::decode(v.value())?;
        if !reminder.done && reminder.due <= until {
            out.push(reminder);
        }
    }
    out.sort_by(|a, b| a.due.cmp(&b.due));
    out.truncate(max);
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

    #[test]
    fn pending_due_by_skips_done_and_future() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-04-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut past = sample(1);
        past.due = DateTime::parse_from_rfc3339("2026-04-01T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut future = sample(2);
        future.due = DateTime::parse_from_rfc3339("2026-04-10T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut done = sample(3);
        done.due = DateTime::parse_from_rfc3339("2026-04-01T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        done.done = true;

        put(&db, &past).unwrap();
        put(&db, &future).unwrap();
        put(&db, &done).unwrap();

        let due = pending_due_by(&db, now, 50).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, 1);
    }

    #[test]
    fn next_due_daily_advances_one_day() {
        let r = Reminder {
            id: 1,
            body: "x".into(),
            due: DateTime::parse_from_rfc3339("2026-06-01T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            recurrence: Some(Recurrence::Daily),
            done: false,
        };
        let next = next_due_after_fire(&r).unwrap();
        assert_eq!(
            next,
            DateTime::parse_from_rfc3339("2026-06-02T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn next_due_none_when_not_recurring() {
        let r = Reminder {
            id: 1,
            body: "x".into(),
            due: Utc::now(),
            recurrence: None,
            done: false,
        };
        assert!(next_due_after_fire(&r).is_none());
    }

    #[test]
    fn pending_due_within_includes_horizon() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let horizon = DateTime::parse_from_rfc3339("2026-04-10T23:59:59Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut r = sample(1);
        r.due = DateTime::parse_from_rfc3339("2026-04-08T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        put(&db, &r).unwrap();

        let list = pending_due_within(&db, horizon, 10).unwrap();
        assert_eq!(list.len(), 1);
    }
}
