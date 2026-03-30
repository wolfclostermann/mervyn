//! Small key-value records in `META_TABLE` (e.g. Slack `event_id` deduplication).
//! Ingest rows are appended first in [`crate::storage::slack_ingest`]; dedupe runs in a later step.

use redb::{Database, ReadableTable};

use super::db::META_TABLE;
use super::error::Result;

const SLACK_EV_PREFIX: &str = "slack:ev:";

fn slack_ev_key(event_id: &str) -> String {
    format!("{SLACK_EV_PREFIX}{event_id}")
}

/// Atomically record that we are handling this Slack `event_id`. Returns `false` if already seen
/// (duplicate delivery — respond 200, do not run side effects again).
pub fn try_claim_slack_delivery(db: &Database, event_id: &str) -> Result<bool> {
    if event_id.is_empty() {
        return Ok(true);
    }
    let key = slack_ev_key(event_id);
    let w = db.begin_write()?;
    let duplicate = {
        let mut t = w.open_table(META_TABLE)?;
        if t.get(key.as_str())?.is_some() {
            true
        } else {
            t.insert(key.as_str(), &[] as &[u8])?;
            false
        }
    };
    w.commit()?;
    if duplicate {
        return Ok(false);
    }
    Ok(true)
}

/// Drop claim so a failed first attempt can be retried by Slack with the same `event_id`.
pub fn release_slack_delivery(db: &Database, event_id: &str) -> Result<()> {
    if event_id.is_empty() {
        return Ok(());
    }
    let key = slack_ev_key(event_id);
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(META_TABLE)?;
        let _ = t.remove(key.as_str())?;
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
    fn claim_then_second_claim_fails() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        assert!(try_claim_slack_delivery(&db, "Ev123").unwrap());
        assert!(!try_claim_slack_delivery(&db, "Ev123").unwrap());
    }

    #[test]
    fn release_allows_reclaim() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        assert!(try_claim_slack_delivery(&db, "Ev456").unwrap());
        release_slack_delivery(&db, "Ev456").unwrap();
        assert!(try_claim_slack_delivery(&db, "Ev456").unwrap());
    }
}
