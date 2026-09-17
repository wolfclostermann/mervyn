//! What the vault and the database last agreed on, and which rows are pending removal from it.
//!
//! Sync needs three inputs to resolve a row, not two. With only the file and the database, a
//! reminder whose box is ticked in redb but clear in Markdown is ambiguous: either the scheduler
//! fired it, or the user un-ticked it, and the two want opposite outcomes. The snapshot — the
//! canonical rendering both sides last held — says which one moved.

use chrono::Utc;
use redb::Database;
use serde::{Deserialize, Serialize};

use super::db::{VAULT_STATE_TABLE, VAULT_TOMBSTONES_TABLE};
use super::error::Result;
use super::table;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultState {
    /// Managed file the row's line lives in.
    pub file: String,
    /// Canonical Markdown for the row as of the last agreed sync.
    pub rendered: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub file: String,
    pub at_ms: i64,
}

pub fn put(db: &Database, id: u64, file: &str, rendered: &str) -> Result<()> {
    table::put_u64(
        db,
        VAULT_STATE_TABLE,
        id,
        &VaultState {
            file: file.to_string(),
            rendered: rendered.to_string(),
        },
    )
}

pub fn get(db: &Database, id: u64) -> Result<Option<VaultState>> {
    table::get_u64(db, VAULT_STATE_TABLE, id)
}

pub fn forget(db: &Database, id: u64) -> Result<bool> {
    table::delete_u64(db, VAULT_STATE_TABLE, id)
}

/// Record that `id` was deleted from the database and its line should go on the next sync.
pub fn tombstone(db: &Database, id: u64, file: &str) -> Result<()> {
    table::put_u64(
        db,
        VAULT_TOMBSTONES_TABLE,
        id,
        &Tombstone {
            file: file.to_string(),
            at_ms: Utc::now().timestamp_millis(),
        },
    )
}

/// Consume the tombstone for `id`, returning whether there was one. Consuming rather than reading
/// means a line removed once is not hunted for ever after.
pub fn take_tombstone(db: &Database, id: u64) -> Result<bool> {
    table::delete_u64(db, VAULT_TOMBSTONES_TABLE, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    fn open_db() -> Database {
        let tmp = NamedTempFile::new().unwrap();
        db::open(tmp.path().to_str().unwrap()).unwrap()
    }

    #[test]
    fn a_snapshot_round_trips_and_can_be_forgotten() {
        let db = open_db();
        put(&db, 1, "reminders.md", "- [ ] One <!--mv:1-->").unwrap();
        put(&db, 2, "events.md", "## 2026-04-05 — Gig\n<!--mv:2-->\n").unwrap();

        assert_eq!(get(&db, 1).unwrap().unwrap().rendered, "- [ ] One <!--mv:1-->");
        assert_eq!(get(&db, 2).unwrap().unwrap().file, "events.md");

        assert!(forget(&db, 1).unwrap());
        assert!(get(&db, 1).unwrap().is_none());
    }

    #[test]
    fn a_tombstone_is_consumed_by_the_first_taker() {
        let db = open_db();
        tombstone(&db, 7, "events.md").unwrap();
        assert!(take_tombstone(&db, 7).unwrap());
        assert!(!take_tombstone(&db, 7).unwrap(), "only the first sync acts on it");
    }
}
