//! Small key-value records in `META_TABLE`: inbound delivery deduplication and the
//! Telegram long-poll cursor.
//! Ingest rows are appended first in [`crate::storage::message_ingest`]; dedupe runs in a later step.

use redb::{Database, ReadableTable};

use super::db::META_TABLE;
use super::error::Result;

const DELIVERY_PREFIX: &str = "tg:update:";

/// Highest `update_id` already confirmed to Telegram; the next poll asks for `offset + 1`.
const POLL_OFFSET_KEY: &str = "tg:poll_offset";

fn delivery_key(delivery_id: &str) -> String {
    format!("{DELIVERY_PREFIX}{delivery_id}")
}

/// Atomically record that we are handling this delivery. Returns `false` if already seen
/// (duplicate — do not run side effects again).
pub fn try_claim_delivery(db: &Database, delivery_id: &str) -> Result<bool> {
    if delivery_id.is_empty() {
        return Ok(true);
    }
    let key = delivery_key(delivery_id);
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

/// Drop the claim so a failed first attempt can be retried on redelivery.
pub fn release_delivery(db: &Database, delivery_id: &str) -> Result<()> {
    if delivery_id.is_empty() {
        return Ok(());
    }
    let key = delivery_key(delivery_id);
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(META_TABLE)?;
        let _ = t.remove(key.as_str())?;
    }
    w.commit()?;
    Ok(())
}

/// Read the stored long-poll cursor, if any.
pub fn get_poll_offset(db: &Database) -> Result<Option<i64>> {
    let r = db.begin_read()?;
    let t = r.open_table(META_TABLE)?;
    let Some(v) = t.get(POLL_OFFSET_KEY)? else {
        return Ok(None);
    };
    let bytes = v.value();
    if bytes.len() != 8 {
        return Ok(None);
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(bytes);
    Ok(Some(i64::from_be_bytes(buf)))
}

/// Persist the long-poll cursor so a restart does not replay already-handled updates.
pub fn set_poll_offset(db: &Database, offset: i64) -> Result<()> {
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(META_TABLE)?;
        t.insert(POLL_OFFSET_KEY, &offset.to_be_bytes() as &[u8])?;
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
        assert!(try_claim_delivery(&db, "884711001").unwrap());
        assert!(!try_claim_delivery(&db, "884711001").unwrap());
    }

    #[test]
    fn release_allows_reclaim() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        assert!(try_claim_delivery(&db, "884711002").unwrap());
        release_delivery(&db, "884711002").unwrap();
        assert!(try_claim_delivery(&db, "884711002").unwrap());
    }

    #[test]
    fn poll_offset_roundtrips_and_defaults_to_none() {
        let tmp = NamedTempFile::new().unwrap();
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(get_poll_offset(&db).unwrap(), None);
        set_poll_offset(&db, 884711001).unwrap();
        assert_eq!(get_poll_offset(&db).unwrap(), Some(884711001));
        set_poll_offset(&db, 884711050).unwrap();
        assert_eq!(get_poll_offset(&db).unwrap(), Some(884711050));
    }

    #[test]
    fn poll_offset_survives_reopen() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let db = db::open(tmp.path().to_str().unwrap()).unwrap();
            set_poll_offset(&db, 42).unwrap();
        }
        let db = db::open(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(get_poll_offset(&db).unwrap(), Some(42));
    }
}
