//! Append-only log of Slack event callback deliveries (every HTTP attempt), then updated in-place
//! with a final [`SlackIngestOutcome`] after dedupe / filter / handler steps.

use std::collections::HashSet;

use chrono::Utc;
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};

use super::codec;
use super::db::SLACK_INGEST_TABLE;
use super::error::Result;
use super::meta;

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

const STALE_PENDING_MSG: &str = "stale Pending (sweeper)";

/// Rows updated by [`sweep_stale_pending`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StalePendingSweepReport {
    pub rewound: usize,
}

/// For each `Pending` row older than `stale_after_minutes`, release the Slack dedupe meta key (if any) and set outcome to [`SlackIngestOutcome::Failed`].
pub fn sweep_stale_pending(
    db: &Database,
    now_ms: i64,
    stale_after_minutes: u32,
) -> Result<StalePendingSweepReport> {
    if stale_after_minutes == 0 {
        return Ok(StalePendingSweepReport::default());
    }
    let threshold_ms = i64::from(stale_after_minutes).saturating_mul(60_000);

    let r = db.begin_read()?;
    let t = r.open_table(SLACK_INGEST_TABLE)?;
    let mut stale: Vec<(u64, SlackIngestEntry)> = Vec::new();
    for row in t.iter()? {
        let (k, v) = row?;
        let id = k.value();
        let Ok(entry) = codec::decode::<SlackIngestEntry>(v.value()) else {
            continue;
        };
        if !matches!(entry.outcome, SlackIngestOutcome::Pending) {
            continue;
        }
        if now_ms.saturating_sub(entry.received_at_ms) <= threshold_ms {
            continue;
        }
        stale.push((id, entry));
    }
    drop(t);
    drop(r);

    if stale.is_empty() {
        return Ok(StalePendingSweepReport::default());
    }

    let mut rewound = 0usize;
    for (id, entry) in stale {
        let _ = meta::release_slack_delivery(db, &entry.event_id);
        set_outcome(db, id, SlackIngestOutcome::Failed(STALE_PENDING_MSG.into()))?;
        rewound += 1;
    }

    Ok(StalePendingSweepReport { rewound })
}

/// Optional filters for [`list_recent`]. All conditions are ANDed. `outcome` matches the variant
/// discriminant (`Pending`, `Processed`, `Failed`, `DuplicateDelivery`, …). `Failed` matches any
/// [`SlackIngestOutcome::Failed`].
#[derive(Debug, Clone, Default)]
pub struct IngestListFilters<'a> {
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub outcome: Option<&'a str>,
    pub event_id: Option<&'a str>,
}

fn outcome_matches_filter(o: &SlackIngestOutcome, filter: &str) -> bool {
    match o {
        SlackIngestOutcome::Failed(_) if filter == "Failed" => true,
        SlackIngestOutcome::Pending if filter == "Pending" => true,
        SlackIngestOutcome::DuplicateDelivery if filter == "DuplicateDelivery" => true,
        SlackIngestOutcome::FilteredBot if filter == "FilteredBot" => true,
        SlackIngestOutcome::FilteredSubtype if filter == "FilteredSubtype" => true,
        SlackIngestOutcome::FilteredUnsupportedType if filter == "FilteredUnsupportedType" => true,
        SlackIngestOutcome::FilteredEmptyText if filter == "FilteredEmptyText" => true,
        SlackIngestOutcome::Processed if filter == "Processed" => true,
        SlackIngestOutcome::Failed(_) => false,
        _ => false,
    }
}

/// Latest rows first (highest table id). Scans the full table in memory — intended for operator
/// debugging, not hot paths. `limit` is clamped to **1..=500**.
pub fn list_recent(
    db: &Database,
    filters: IngestListFilters<'_>,
    limit: usize,
) -> Result<Vec<(u64, SlackIngestEntry)>> {
    let limit = limit.clamp(1, 500);
    let r = db.begin_read()?;
    let t = r.open_table(SLACK_INGEST_TABLE)?;
    let mut rows: Vec<(u64, SlackIngestEntry)> = Vec::new();
    for row in t.iter()? {
        let (k, v) = row?;
        let id = k.value();
        let Ok(entry) = codec::decode::<SlackIngestEntry>(v.value()) else {
            continue;
        };
        if let Some(s) = filters.since_ms {
            if entry.received_at_ms < s {
                continue;
            }
        }
        if let Some(u) = filters.until_ms {
            if entry.received_at_ms > u {
                continue;
            }
        }
        if let Some(eid) = filters.event_id {
            if entry.event_id != eid {
                continue;
            }
        }
        if let Some(ov) = filters.outcome {
            if !outcome_matches_filter(&entry.outcome, ov) {
                continue;
            }
        }
        rows.push((id, entry));
    }
    drop(t);
    drop(r);
    rows.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
    rows.truncate(limit);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use crate::storage::meta;
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

    #[test]
    fn sweep_stale_pending_releases_meta_and_sets_failed() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        assert!(meta::try_claim_slack_delivery(&db, "EvStale").unwrap());

        let now_ms = 1_800_000_000_000_i64;
        let old_ms = now_ms - 120 * 60_000;
        put(
            &db,
            1,
            &SlackIngestEntry {
                event_id: "EvStale".into(),
                received_at_ms: old_ms,
                retry_num: None,
                inner_type: "message".into(),
                outcome: SlackIngestOutcome::Pending,
            },
        )
        .unwrap();

        let r = sweep_stale_pending(&db, now_ms, 30).unwrap();
        assert_eq!(r.rewound, 1);
        assert!(meta::try_claim_slack_delivery(&db, "EvStale").unwrap());

        let read = db.begin_read().unwrap();
        let t = read.open_table(SLACK_INGEST_TABLE).unwrap();
        let e: SlackIngestEntry = codec::decode(t.get(1).unwrap().unwrap().value()).unwrap();
        assert!(matches!(
            e.outcome,
            SlackIngestOutcome::Failed(ref s) if s == STALE_PENDING_MSG
        ));
    }

    #[test]
    fn sweep_respects_zero_disable() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        put(
            &db,
            1,
            &SlackIngestEntry {
                event_id: "Ev".into(),
                received_at_ms: 0,
                retry_num: None,
                inner_type: "message".into(),
                outcome: SlackIngestOutcome::Pending,
            },
        )
        .unwrap();
        let r = sweep_stale_pending(&db, 9_999_999_999_999, 0).unwrap();
        assert_eq!(r.rewound, 0);
    }

    #[test]
    fn list_recent_filters_and_orders_newest_first() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        put(
            &db,
            1,
            &SlackIngestEntry {
                event_id: "A".into(),
                received_at_ms: 100,
                retry_num: None,
                inner_type: "message".into(),
                outcome: SlackIngestOutcome::Pending,
            },
        )
        .unwrap();
        put(
            &db,
            2,
            &SlackIngestEntry {
                event_id: "B".into(),
                received_at_ms: 200,
                retry_num: None,
                inner_type: "message".into(),
                outcome: SlackIngestOutcome::Processed,
            },
        )
        .unwrap();
        let rows = list_recent(
            &db,
            IngestListFilters {
                outcome: Some("Processed"),
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 2);
        assert_eq!(rows[0].1.event_id, "B");

        let rows = list_recent(
            &db,
            IngestListFilters {
                event_id: Some("A"),
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 1);

        let rows = list_recent(
            &db,
            IngestListFilters {
                since_ms: Some(150),
                until_ms: Some(250),
                ..Default::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 2);
    }
}
