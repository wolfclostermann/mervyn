//! What the vault and the database last agreed on, and which rows are pending removal from it.
//!
//! Sync needs three inputs to resolve a row, not two. With only the file and the database, a
//! reminder whose box is ticked in redb but clear in Markdown is ambiguous: either the scheduler
//! fired it, or the user un-ticked it, and the two want opposite outcomes. The snapshot — the
//! canonical rendering both sides last held — says which one moved.

use chrono::Utc;
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::{VAULT_STATE_TABLE, VAULT_TOMBSTONES_TABLE};
use super::error::Result;


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

/// Scope a row id to the file it lives in. Ids repeat across tables; the pair does not.
fn key(file: &str, id: u64) -> String {
    format!("{file}#{id:x}")
}

fn put_str<T: serde::Serialize>(
    db: &Database,
    table: redb::TableDefinition<&str, &[u8]>,
    k: &str,
    value: &T,
) -> Result<()> {
    let bytes = codec::encode(value)?;
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(table)?;
        t.insert(k, bytes.as_slice())?;
    }
    w.commit()?;
    Ok(())
}

fn get_str<T: serde::de::DeserializeOwned>(
    db: &Database,
    table: redb::TableDefinition<&str, &[u8]>,
    k: &str,
) -> Result<Option<T>> {
    let r = db.begin_read()?;
    let t = r.open_table(table)?;
    let Some(guard) = t.get(k)? else {
        return Ok(None);
    };
    Ok(Some(codec::decode(guard.value())?))
}

fn delete_str(
    db: &Database,
    table: redb::TableDefinition<&str, &[u8]>,
    k: &str,
) -> Result<bool> {
    let w = db.begin_write()?;
    let removed = {
        let mut t = w.open_table(table)?;
        let old = t.remove(k)?;
        old.is_some()
    };
    w.commit()?;
    Ok(removed)
}

pub fn put(db: &Database, id: u64, file: &str, rendered: &str) -> Result<()> {
    put_str(
        db,
        VAULT_STATE_TABLE,
        &key(file, id),
        &VaultState {
            file: file.to_string(),
            rendered: rendered.to_string(),
        },
    )
}

pub fn get(db: &Database, id: u64, file: &str) -> Result<Option<VaultState>> {
    get_str(db, VAULT_STATE_TABLE, &key(file, id))
}

pub fn forget(db: &Database, id: u64, file: &str) -> Result<bool> {
    delete_str(db, VAULT_STATE_TABLE, &key(file, id))
}

/// Record that `id` was deleted from the database and its line should go on the next sync.
pub fn tombstone(db: &Database, id: u64, file: &str) -> Result<()> {
    put_str(
        db,
        VAULT_TOMBSTONES_TABLE,
        &key(file, id),
        &Tombstone {
            file: file.to_string(),
            at_ms: Utc::now().timestamp_millis(),
        },
    )
}

/// Whether a line for `id` is still waiting to be removed from `file`.
pub fn has_tombstone(db: &Database, id: u64, file: &str) -> Result<bool> {
    Ok(get_str::<Tombstone>(db, VAULT_TOMBSTONES_TABLE, &key(file, id))?.is_some())
}

/// Every line still waiting to be removed, as `(file, id)`. For the operator view: a tombstone
/// that never clears means a write has been failing quietly.
pub fn pending_tombstones(db: &Database) -> Result<Vec<(String, u64)>> {
    let r = db.begin_read()?;
    let t = r.open_table(VAULT_TOMBSTONES_TABLE)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (k, v) = row?;
        let stone: Tombstone = codec::decode(v.value())?;
        // Key is `file#hexid`; the id is easier to recover from the key than to store twice.
        let id = k
            .value()
            .rsplit_once('#')
            .and_then(|(_, hex)| u64::from_str_radix(hex, 16).ok())
            .unwrap_or(0);
        out.push((stone.file, id));
    }
    Ok(out)
}

/// Consume the tombstone for `id`, returning whether there was one.
///
/// Called only once the line has actually gone from the file on disk. Consuming it during the
/// merge would mean a write that then failed — or a commit later discarded — left no record that
/// the section still needs removing, and the next sync would read the surviving line as a row to
/// re-import.
pub fn take_tombstone(db: &Database, id: u64, file: &str) -> Result<bool> {
    delete_str(db, VAULT_TOMBSTONES_TABLE, &key(file, id))
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
    fn the_same_id_in_two_files_is_two_snapshots() {
        let db = open_db();
        put(&db, 1, "events.md", "## 2026-04-05 — Gig\n<!--mv:1-->\n").unwrap();
        put(&db, 1, "todos.md", "- [ ] Call the plumber <!--mv:1-->").unwrap();

        assert_eq!(get(&db, 1, "events.md").unwrap().unwrap().file, "events.md");
        assert_eq!(get(&db, 1, "todos.md").unwrap().unwrap().file, "todos.md");
        assert!(get(&db, 1, "reminders.md").unwrap().is_none());
    }

    #[test]
    fn a_snapshot_round_trips_and_can_be_forgotten() {
        let db = open_db();
        put(&db, 1, "reminders.md", "- [ ] One <!--mv:1-->").unwrap();
        put(&db, 2, "events.md", "## 2026-04-05 — Gig\n<!--mv:2-->\n").unwrap();

        assert_eq!(
            get(&db, 1, "reminders.md").unwrap().unwrap().rendered,
            "- [ ] One <!--mv:1-->"
        );
        assert_eq!(get(&db, 2, "events.md").unwrap().unwrap().file, "events.md");

        assert!(forget(&db, 1, "reminders.md").unwrap());
        assert!(get(&db, 1, "reminders.md").unwrap().is_none());
    }

    #[test]
    fn a_tombstone_is_consumed_by_the_first_taker() {
        let db = open_db();
        tombstone(&db, 7, "events.md").unwrap();
        assert!(has_tombstone(&db, 7, "events.md").unwrap());
        assert!(!has_tombstone(&db, 7, "todos.md").unwrap());
        assert!(!take_tombstone(&db, 7, "todos.md").unwrap(), "another file's id 7 is not this one");
        assert!(take_tombstone(&db, 7, "events.md").unwrap());
        assert!(
            !take_tombstone(&db, 7, "events.md").unwrap(),
            "only the first sync acts on it"
        );
    }
}
