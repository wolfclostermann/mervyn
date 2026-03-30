//! Append-only log of Slack event callback deliveries (every HTTP attempt), then updated in-place
//! with a final [`SlackIngestOutcome`] after dedupe / filter / handler steps.

use std::collections::HashSet;

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

pub(super) fn put(db: &Database, id: u64, entry: &SlackIngestEntry) -> Result<()> {
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

/// Rows removed by [`prune`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub removed_by_age: usize,
    pub removed_by_cap: usize,
}

impl PruneReport {
    pub fn total_removed(&self) -> usize {
        self.removed_by_age + self.removed_by_cap
    }
}

/// Delete old or excess `slack_ingest` rows. `retention_days` / `keep_last` of `None` or `Some(0)` disable that rule.
pub fn prune(
    db: &Database,
    now_ms: i64,
    retention_days: Option<u32>,
    keep_last: Option<u64>,
) -> Result<PruneReport> {
    let retention_days = retention_days.and_then(|d| (d > 0).then_some(d));
    let keep_last = keep_last.and_then(|k| (k > 0).then_some(k));

    if retention_days.is_none() && keep_last.is_none() {
        return Ok(PruneReport::default());
    }

    let r = db.begin_read()?;
    let t = r.open_table(SLACK_INGEST_TABLE)?;
    let mut rows: Vec<(u64, SlackIngestEntry)> = Vec::new();
    for row in t.iter()? {
        let (k, v) = row?;
        let id = k.value();
        match codec::decode(v.value()) {
            Ok(e) => rows.push((id, e)),
            Err(e) => tracing::warn!(row_id = id, error = %e, "slack_ingest prune: skip corrupt row"),
        }
    }
    drop(t);
    drop(r);

    let mut by_age = HashSet::new();
    if let Some(days) = retention_days {
        let cutoff = now_ms.saturating_sub(i64::from(days).saturating_mul(86_400_000));
        for (id, e) in &rows {
            if e.received_at_ms < cutoff {
                by_age.insert(*id);
            }
        }
    }

    let mut survivor_ids: Vec<u64> = rows
        .iter()
        .filter(|(id, _)| !by_age.contains(id))
        .map(|(id, _)| *id)
        .collect();
    survivor_ids.sort_unstable();

    let mut by_cap = HashSet::new();
    if let Some(max) = keep_last {
        let n = survivor_ids.len() as u64;
        if n > max {
            let remove_n = (n - max) as usize;
            for id in survivor_ids.iter().take(remove_n) {
                by_cap.insert(*id);
            }
        }
    }

    let to_delete: HashSet<u64> = by_age.union(&by_cap).copied().collect();
    if to_delete.is_empty() {
        return Ok(PruneReport::default());
    }

    let w = db.begin_write()?;
    {
        let mut tbl = w.open_table(SLACK_INGEST_TABLE)?;
        for id in &to_delete {
            let _ = tbl.remove(*id)?;
        }
    }
    w.commit()?;

    Ok(PruneReport {
        removed_by_age: by_age.len(),
        removed_by_cap: by_cap.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::NamedTempFile;

    fn sample_entry(received_at_ms: i64) -> SlackIngestEntry {
        SlackIngestEntry {
            event_id: "Ev".into(),
            received_at_ms,
            retry_num: None,
            inner_type: "message".into(),
            outcome: SlackIngestOutcome::Processed,
        }
    }

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

    #[test]
    fn prune_removes_rows_older_than_retention() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let now_ms = 1_700_000_000_000_i64;
        let old_ms = now_ms - 100_i64 * 86_400_000;
        put(&db, 1, &sample_entry(old_ms)).unwrap();
        put(&db, 2, &sample_entry(now_ms)).unwrap();
        let r = prune(&db, now_ms, Some(90), None).unwrap();
        assert_eq!(r.removed_by_age, 1);
        assert_eq!(r.removed_by_cap, 0);
        let read = db.begin_read().unwrap();
        let t = read.open_table(SLACK_INGEST_TABLE).unwrap();
        assert!(t.get(1).unwrap().is_none());
        assert!(t.get(2).unwrap().is_some());
    }

    #[test]
    fn prune_enforces_keep_last() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        let now_ms = 1_700_000_000_000_i64;
        for id in 1..=5_u64 {
            put(&db, id, &sample_entry(now_ms)).unwrap();
        }
        let r = prune(&db, now_ms, None, Some(2)).unwrap();
        assert_eq!(r.removed_by_age, 0);
        assert_eq!(r.removed_by_cap, 3);
        let read = db.begin_read().unwrap();
        let t = read.open_table(SLACK_INGEST_TABLE).unwrap();
        assert!(t.get(1).unwrap().is_none());
        assert!(t.get(2).unwrap().is_none());
        assert!(t.get(3).unwrap().is_none());
        assert!(t.get(4).unwrap().is_some());
        assert!(t.get(5).unwrap().is_some());
    }

    #[test]
    fn prune_noop_when_rules_disabled() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        put(&db, 1, &sample_entry(0)).unwrap();
        let r = prune(&db, 9_999_999_999_999, None, None).unwrap();
        assert_eq!(r.total_removed(), 0);
        let r = prune(&db, 9_999_999_999_999, Some(0), Some(0)).unwrap();
        assert_eq!(r.total_removed(), 0);
    }
}
