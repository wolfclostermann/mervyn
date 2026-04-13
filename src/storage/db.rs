use redb::{Database, TableDefinition};

pub const EVENTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("events");

pub const REMINDERS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("reminders");

pub const WORKLOG_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("worklog");

pub const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

pub const SLACK_INGEST_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("slack_ingest");

pub const TODOS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("todos");

/// Tracks Slack pings for calendar events (advance + start), keyed by `event.id`.
pub const EVENT_NOTICES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("event_notices");

pub fn open(path: &str) -> anyhow::Result<Database> {
    let db = Database::create(path)?;
    let write_txn = db.begin_write()?;
    {
        let _ = write_txn.open_table(EVENTS_TABLE)?;
        let _ = write_txn.open_table(REMINDERS_TABLE)?;
        let _ = write_txn.open_table(WORKLOG_TABLE)?;
        let _ = write_txn.open_table(META_TABLE)?;
        let _ = write_txn.open_table(SLACK_INGEST_TABLE)?;
        let _ = write_txn.open_table(TODOS_TABLE)?;
        let _ = write_txn.open_table(EVENT_NOTICES_TABLE)?;
    }
    write_txn.commit()?;
    Ok(db)
}
