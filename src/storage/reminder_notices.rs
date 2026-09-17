//! Per-reminder record of an occurrence already announced but not yet settled in the row.
//!
//! Sending a Telegram message and advancing the reminder are two steps and cannot be made one:
//! `sendMessage` has no idempotency key, so whichever order they run in, a failure between them
//! loses something. Sending first risks a duplicate; persisting first risks a reminder that never
//! arrives. For a personal assistant a duplicate beats a miss, so the sweep sends first — and this
//! table is what stops "a duplicate" becoming "one every minute for ever" when the row write is
//! the thing that keeps failing.

use redb::Database;
use serde::{Deserialize, Serialize};

use super::db::REMINDER_NOTICES_TABLE;
use super::error::Result;
use super::table;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderNoticeState {
    /// The `due` this refers to. A reminder that moves — rescheduled in the vault, say — gets a
    /// fresh count rather than inheriting one from an occurrence that has passed.
    pub due_unix: i64,
    /// How many times this same occurrence has been announced without the row advancing.
    pub sends: u32,
}

pub fn put(db: &Database, reminder_id: u64, state: &ReminderNoticeState) -> Result<()> {
    table::put_u64(db, REMINDER_NOTICES_TABLE, reminder_id, state)
}

pub fn get(db: &Database, reminder_id: u64) -> Result<Option<ReminderNoticeState>> {
    table::get_u64(db, REMINDER_NOTICES_TABLE, reminder_id)
}

pub fn delete(db: &Database, reminder_id: u64) -> Result<bool> {
    table::delete_u64(db, REMINDER_NOTICES_TABLE, reminder_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    #[test]
    fn put_get_delete() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let st = ReminderNoticeState {
            due_unix: 1_700_000_000,
            sends: 2,
        };
        put(&db, 7, &st).unwrap();
        assert_eq!(get(&db, 7).unwrap(), Some(st));
        assert!(delete(&db, 7).unwrap());
        assert_eq!(get(&db, 7).unwrap(), None);
    }
}
