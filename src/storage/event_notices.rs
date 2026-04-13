//! Per-event flags for appointment Slack reminders (advance + start).

use redb::Database;
use serde::{Deserialize, Serialize};

use super::db::EVENT_NOTICES_TABLE;
use super::error::Result;
use super::table;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventNoticeState {
    /// `event.start` at the time flags were last aligned; if the event moves, flags reset.
    pub start_unix: i64,
    pub advance_sent: bool,
    pub start_sent: bool,
}

pub fn put(db: &Database, event_id: u64, state: &EventNoticeState) -> Result<()> {
    table::put_u64(db, EVENT_NOTICES_TABLE, event_id, state)
}

pub fn get(db: &Database, event_id: u64) -> Result<Option<EventNoticeState>> {
    table::get_u64(db, EVENT_NOTICES_TABLE, event_id)
}

pub fn delete(db: &Database, event_id: u64) -> Result<bool> {
    table::delete_u64(db, EVENT_NOTICES_TABLE, event_id)
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
        let st = EventNoticeState {
            start_unix: 1_700_000_000,
            advance_sent: true,
            start_sent: false,
        };
        put(&db, 7, &st).unwrap();
        assert_eq!(get(&db, 7).unwrap(), Some(st));
        assert!(delete(&db, 7).unwrap());
        assert_eq!(get(&db, 7).unwrap(), None);
    }
}
