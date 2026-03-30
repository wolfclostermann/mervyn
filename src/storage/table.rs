//! Shared postcard encode/decode + transaction patterns for `TableDefinition<u64, &[u8]>`.
//! Domain modules keep query-specific scans; CRUD stays DRY here.

use redb::{Database, ReadableTable, TableDefinition};
use serde::{de::DeserializeOwned, Serialize};

use super::codec;
use super::error::Result;

pub fn put_u64<T: Serialize>(
    db: &Database,
    table: TableDefinition<u64, &[u8]>,
    id: u64,
    value: &T,
) -> Result<()> {
    let bytes = codec::encode(value)?;
    let w = db.begin_write()?;
    {
        let mut t = w.open_table(table)?;
        t.insert(id, bytes.as_slice())?;
    }
    w.commit()?;
    Ok(())
}

pub fn get_u64<T: DeserializeOwned>(
    db: &Database,
    table: TableDefinition<u64, &[u8]>,
    id: u64,
) -> Result<Option<T>> {
    let r = db.begin_read()?;
    let t = r.open_table(table)?;
    let Some(guard) = t.get(id)? else {
        return Ok(None);
    };
    Ok(Some(codec::decode(guard.value())?))
}

/// Returns whether a row existed and was removed.
pub fn delete_u64(
    db: &Database,
    table: TableDefinition<u64, &[u8]>,
    id: u64,
) -> Result<bool> {
    let w = db.begin_write()?;
    let removed = {
        let mut t = w.open_table(table)?;
        let old = t.remove(id)?;
        old.is_some()
    };
    w.commit()?;
    Ok(removed)
}

/// Full table scan; rows decoded and sorted by `sort_key` ascending.
pub fn list_all_u64<T: DeserializeOwned>(
    db: &Database,
    table: TableDefinition<u64, &[u8]>,
    sort_key: impl Fn(&T) -> u64,
) -> Result<Vec<T>> {
    let r = db.begin_read()?;
    let t = r.open_table(table)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (_, v) = row?;
        out.push(codec::decode(v.value())?);
    }
    out.sort_by_key(|x| sort_key(x));
    Ok(out)
}

/// Max primary key + 1 (or 1 if empty). For app-assigned ids; vault sync uses stable hashes.
pub fn next_id_u64(db: &Database, table: TableDefinition<u64, &[u8]>) -> Result<u64> {
    let r = db.begin_read()?;
    let t = r.open_table(table)?;
    let mut max = 0u64;
    for row in t.iter()? {
        let (k, _) = row?;
        max = max.max(k.value());
    }
    Ok(max.saturating_add(1))
}
