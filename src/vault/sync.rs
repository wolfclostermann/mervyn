//! Parse Obsidian vault Markdown and upsert into redb.

use std::path::Path;

use redb::Database;

use super::md;
use crate::storage::{events, reminders, worklog};

#[derive(Debug, Default, Clone)]
pub struct SyncStats {
    pub reminders: usize,
    pub events: usize,
    pub worklog_entries: usize,
}

/// Read `reminders.md`, `events.md`, and `worklog.md` under `vault_path` and upsert rows.
pub fn sync_vault_to_db(db: &Database, vault_path: &Path) -> anyhow::Result<SyncStats> {
    let mut stats = SyncStats::default();

    let reminders_path = vault_path.join("reminders.md");
    if reminders_path.exists() {
        let text = std::fs::read_to_string(&reminders_path)?;
        for r in md::parse_reminders(&text) {
            reminders::put(db, &r).map_err(|e| anyhow::anyhow!(e))?;
            stats.reminders += 1;
        }
    }

    let events_path = vault_path.join("events.md");
    if events_path.exists() {
        let text = std::fs::read_to_string(&events_path)?;
        for e in md::parse_events(&text) {
            events::put(db, &e).map_err(|e| anyhow::anyhow!(e))?;
            stats.events += 1;
        }
    }

    let worklog_path = vault_path.join("worklog.md");
    if worklog_path.exists() {
        let text = std::fs::read_to_string(&worklog_path)?;
        for w in md::parse_worklog(&text) {
            worklog::put(db, &w).map_err(|e| anyhow::anyhow!(e))?;
            stats.worklog_entries += 1;
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::tempdir;

    #[test]
    fn parse_reminder_due_and_recurrence() {
        let md = "# Reminders\n\n- [ ] Pay tax — due 2026-04-10 — recurs yearly\n";
        let list = md::parse_reminders(md);
        assert_eq!(list.len(), 1);
        assert!(!list[0].done);
        assert_eq!(list[0].body, "Pay tax");
        assert!(matches!(
            list[0].recurrence,
            Some(crate::storage::Recurrence::Custom(ref s)) if s == "yearly"
        ));
    }

    #[test]
    fn sync_roundtrip_to_db() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("t.redb");
        let db = db::open(db_path.to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        std::fs::write(
            vault.path().join("reminders.md"),
            "- [ ] Test — due 2026-05-01\n",
        )
        .unwrap();

        let s = sync_vault_to_db(&db, vault.path()).unwrap();
        assert_eq!(s.reminders, 1);
        let all = reminders::list_all(&db).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].body, "Test");
    }
}
