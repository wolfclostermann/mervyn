//! Append-only log of Slack event callback deliveries (every HTTP attempt), then updated in-place
//! with a final [`SlackIngestOutcome`] after dedupe / filter / handler steps.

use chrono::Utc;
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::SLACK_INGEST_TABLE;
use super::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlackIngestOutcome {
    /// Written on append; should be replaced before the worker returns.
    Pending,
    DuplicateDelivery,
    FilteredBot,
    FilteredSubtype,
    FilteredUnsupportedType,
    FilteredEmptyText,
    Processed,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackIngestEntry {
    pub event_id: String,
    pub received_at_ms: i64,
    pub retry_num: Option<u32>,
    pub inner_type: String,
    pub outcome: SlackIngestOutcome,
}

fn next_id(db: &Database) -> Result<u64> {
    let r = db.begin_read()?;
    let t = r.open_table(SLACK_INGEST_TABLE)?;
    let mut max = 0u64;
    for row in t.iter()? {
        let (k, _) = row?;
        max = max.max(k.value());
    }
    Ok(max.saturating_add(1))
}

/// Record one delivery attempt. Always call [`set_outcome`] (or leave `Pending` only on panic).
pub fn append(
    db: &Database,
    event_id: String,
    retry_num: Option<u32>,
    inner_type: String,
) -> Result<u64> {
    let id = next_id(db)?;
    let entry = SlackIngestEntry {
        event_id,
        received_at_ms: Utc::now().timestamp_millis(),
        retry_num,
        inner_type,
        outcome: SlackIngestOutcome::Pending,
    };
    put(db, id, &entry)?;
    Ok(id)
}

fn put(db: &Database, id: u64, entry: &SlackIngestEntry) -> Result<()> {
    let bytes = codec::encode(entry)?;
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(SLACK_INGEST_TABLE)?;
        t.insert(id, bytes.as_slice())?;
    }
    w.commit()?;
    Ok(())
}

pub fn set_outcome(db: &Database, id: u64, outcome: SlackIngestOutcome) -> Result<()> {
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(SLACK_INGEST_TABLE)?;
        let raw: Option<Vec<u8>> = t.get(id)?.map(|g| g.value().to_vec());
        let Some(raw) = raw else {
            drop(t);
            w.commit()?;
            return Ok(());
        };
        let mut entry: SlackIngestEntry = codec::decode(&raw)?;
        entry.outcome = outcome;
        let bytes = codec::encode(&entry)?;
        t.insert(id, bytes.as_slice())?;
    }
    w.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    #[test]
    fn append_then_set_outcome() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let id = append(&db, "Ev1".into(), Some(0), "message".into()).unwrap();
        set_outcome(&db, id, SlackIngestOutcome::Processed).unwrap();
        let r = db.begin_read().unwrap();
        let t = r.open_table(SLACK_INGEST_TABLE).unwrap();
        let g = t.get(id).unwrap().unwrap();
        let e: SlackIngestEntry = codec::decode(g.value()).unwrap();
        assert_eq!(e.outcome, SlackIngestOutcome::Processed);
        assert_eq!(e.retry_num, Some(0));
    }
}
