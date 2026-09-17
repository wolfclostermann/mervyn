use redb::{Database, TableDefinition};

pub const EVENTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("events");

pub const REMINDERS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("reminders");

pub const WORKLOG_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("worklog");

pub const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

pub const MESSAGE_INGEST_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("message_ingest");

pub const TODOS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("todos");

/// Tracks chat pings for calendar events (advance + start), keyed by `event.id`.
pub const EVENT_NOTICES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("event_notices");

/// What the vault file and the database last agreed on. The evidence that lets a sync tell a
/// human's edit from the scheduler's own change — see `docs/two-way-vault-sync.md`.
///
/// Keyed by `file#id`, **not** by id alone: primary keys are unique within their own table, so an
/// event and a todo are both routinely id 1. Keying by id let one shadow the other's snapshot.
pub const VAULT_STATE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("vault_state_by_file");

/// Rows deleted from the database that still have a line in the vault. Explicit, because
/// inferring a delete from absence would empty the vault the first time a backup was restored.
/// Keyed by `file#id` for the same reason as above.
pub const VAULT_TOMBSTONES_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("vault_tombstones_by_file");

pub fn open(path: &str) -> anyhow::Result<Database> {
    let db = Database::create(path)?;
    let write_txn = db.begin_write()?;
    {
        let _ = write_txn.open_table(EVENTS_TABLE)?;
        let _ = write_txn.open_table(REMINDERS_TABLE)?;
        let _ = write_txn.open_table(WORKLOG_TABLE)?;
        let _ = write_txn.open_table(META_TABLE)?;
        let _ = write_txn.open_table(MESSAGE_INGEST_TABLE)?;
        let _ = write_txn.open_table(TODOS_TABLE)?;
        let _ = write_txn.open_table(EVENT_NOTICES_TABLE)?;
        let _ = write_txn.open_table(VAULT_STATE_TABLE)?;
        let _ = write_txn.open_table(VAULT_TOMBSTONES_TABLE)?;
    }
    write_txn.commit()?;
    Ok(db)
}
