//! Parse Obsidian vault Markdown and upsert into redb, then write id markers back.
//!
//! The Markdown → redb direction always runs. The redb → Markdown direction is gated on
//! [`WriteBackPolicy`] and, at this phase, consists only of back-filling the id markers that give
//! each item a stable identity. See `docs/two-way-vault-sync.md`.

use std::path::{Path, PathBuf};

use chrono_tz::Tz;
use redb::Database;

use super::md::{self, FoundItem};
use super::write::{splice, VaultAccess};
use crate::storage::{events, reminders, worklog};

/// Files Mervyn may write to. `worklog.md` is deliberately absent: it is a symlink into the
/// worklog clone, and modifying that working tree would break `git pull --ff-only`.
const MANAGED_FILES: [&str; 2] = ["reminders.md", "events.md"];

/// A file whose markers are ready to write: its path, the bytes they were measured against, and
/// the insertions themselves.
type PendingWrite = (PathBuf, String, Vec<(usize, String)>);

#[derive(Debug, Default, Clone)]
pub struct SyncStats {
    pub reminders: usize,
    pub events: usize,
    pub worklog_entries: usize,
    /// Items that had no id marker and were given one on this pass.
    pub markers_added: usize,
}

/// Whether this sync may modify the vault, and what to do before its first write.
#[derive(Debug, Clone, Copy)]
pub struct WriteBackPolicy {
    pub enabled: bool,
    pub backup_before_first_write: bool,
}

/// Everything one sync cycle needs beyond the database and the vault path.
#[derive(Clone, Copy)]
pub struct SyncContext<'a> {
    pub access: &'a VaultAccess,
    pub policy: WriteBackPolicy,
    /// Wall-clock zone the vault is written and read in — `scheduler.timezone`.
    pub tz: Tz,
}

impl WriteBackPolicy {
    /// The historical behaviour: import only, never touch the files.
    #[allow(dead_code)] // Production reads the policy from config; this names the default.
    pub const READ_ONLY: Self = Self {
        enabled: false,
        backup_before_first_write: false,
    };
}

fn read_if_present(path: &Path) -> anyhow::Result<Option<String>> {
    // `exists()` follows symlinks, so a `worklog.md` pointing at an in-container path reads as
    // absent on the host. That is the intended behaviour, and the reason it is spelled out here.
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(path)?))
}

fn marker_edits<T>(found: &[FoundItem<T>], id_of: impl Fn(&T) -> u64) -> Vec<(usize, String)> {
    found
        .iter()
        .filter_map(|f| f.backfill(id_of(&f.item)))
        .collect()
}

/// Read `reminders.md`, `events.md` and `worklog.md` under `vault_path` and upsert rows; then,
/// if `policy` allows, splice an id marker into every item that lacks one.
///
/// Takes the vault lock for the whole cycle: parse, upsert and write have to be one operation, or
/// a concurrent sync could write markers derived from a file another cycle has already changed.
pub fn sync_vault_to_db(
    db: &Database,
    vault_path: &Path,
    ctx: SyncContext<'_>,
) -> anyhow::Result<SyncStats> {
    let SyncContext { access, policy, tz } = ctx;
    let _guard = access.lock();
    let mut stats = SyncStats::default();
    let mut pending: Vec<PendingWrite> = Vec::new();

    let reminders_path = vault_path.join("reminders.md");
    if let Some(raw) = read_if_present(&reminders_path)? {
        let found = md::find_reminders(&raw, tz);
        for f in &found {
            reminders::put(db, &f.item).map_err(|e| anyhow::anyhow!(e))?;
            stats.reminders += 1;
        }
        let edits = marker_edits(&found, |r| r.id);
        if policy.enabled && !edits.is_empty() {
            pending.push((reminders_path, raw, edits));
        }
    }

    let events_path = vault_path.join("events.md");
    if let Some(raw) = read_if_present(&events_path)? {
        let found = md::find_events(&raw, tz);
        for f in &found {
            events::put(db, &f.item).map_err(|e| anyhow::anyhow!(e))?;
            stats.events += 1;
        }
        let edits = marker_edits(&found, |e| e.id);
        if policy.enabled && !edits.is_empty() {
            pending.push((events_path, raw, edits));
        }
    }

    let worklog_path = vault_path.join("worklog.md");
    if let Some(raw) = read_if_present(&worklog_path)? {
        for w in md::parse_worklog(&raw, tz) {
            worklog::put(db, &w).map_err(|e| anyhow::anyhow!(e))?;
            stats.worklog_entries += 1;
        }
    }

    if pending.is_empty() {
        return Ok(stats);
    }

    if policy.backup_before_first_write {
        match access.backup_once(vault_path, &MANAGED_FILES, chrono::Utc::now()) {
            Ok(Some(dir)) => tracing::info!(dir = %dir.display(), "vault backed up before first write-back"),
            Ok(None) => {}
            Err(e) => {
                // A write-back that cannot be undone is not worth the convenience.
                anyhow::bail!("vault backup failed, refusing to write: {e}");
            }
        }
    }

    for (path, raw, mut edits) in pending {
        let added = edits.len();
        let updated = splice(&raw, &mut edits);

        // Optimistic concurrency: if Obsidian flushed a buffer while we were parsing, the offsets
        // we measured no longer describe this file. Skip it; the next tick re-reads and retries.
        match read_if_present(&path)? {
            Some(current) if current == raw => {}
            _ => {
                tracing::warn!(path = %path.display(), "vault changed during sync; markers deferred");
                continue;
            }
        }

        access.write_file(&path, &updated)?;
        stats.markers_added += added;
        tracing::info!(path = %path.display(), added, "id markers written");
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use crate::vault::marker;
    use tempfile::tempdir;

    fn london() -> Tz {
        "Europe/London".parse().unwrap()
    }

    fn ctx(access: &VaultAccess, enabled: bool) -> SyncContext<'_> {
        SyncContext {
            access,
            policy: WriteBackPolicy {
                enabled,
                backup_before_first_write: false,
            },
            tz: london(),
        }
    }

    #[test]
    fn parse_reminder_due_and_recurrence() {
        let md = "# Reminders\n\n- [ ] Pay tax — due 2026-04-10 — recurs yearly\n";
        let list = md::parse_reminders(md, london());
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
        let db = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        std::fs::write(
            vault.path().join("reminders.md"),
            "- [ ] Test — due 2026-05-01\n",
        )
        .unwrap();

        let access = VaultAccess::new();
        let s = sync_vault_to_db(&db, vault.path(), ctx(&access, false)).unwrap();
        assert_eq!(s.reminders, 1);
        let all = reminders::list_all(&db).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].body, "Test");
    }

    #[test]
    fn read_only_policy_leaves_the_file_untouched() {
        let dir = tempdir().unwrap();
        let db = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        let path = vault.path().join("reminders.md");
        let before = "- [ ] Test — due 2026-05-01\n";
        std::fs::write(&path, before).unwrap();

        let access = VaultAccess::new();
        let s = sync_vault_to_db(&db, vault.path(), ctx(&access, false)).unwrap();

        assert_eq!(s.markers_added, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn write_back_marks_every_item_once_and_then_stops() {
        let dir = tempdir().unwrap();
        let db = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        std::fs::write(
            vault.path().join("reminders.md"),
            "- [ ] One — due 2026-05-01\n- [x] Two\n",
        )
        .unwrap();
        std::fs::write(
            vault.path().join("events.md"),
            "## 2026-04-05 — Gig\nDoors 7pm\n",
        )
        .unwrap();

        let access = VaultAccess::new();
        let first = sync_vault_to_db(&db, vault.path(), ctx(&access, true)).unwrap();
        assert_eq!(first.markers_added, 3, "two reminders and one event");

        let second = sync_vault_to_db(&db, vault.path(), ctx(&access, true)).unwrap();
        assert_eq!(second.markers_added, 0, "second pass has nothing to add");

        let reminders_md = std::fs::read_to_string(vault.path().join("reminders.md")).unwrap();
        for r in reminders::list_all(&db).unwrap() {
            assert!(
                reminders_md.contains(&marker::render(r.id)),
                "row {} has no marker in the file:\n{reminders_md}",
                r.id
            );
        }
    }

    #[test]
    fn an_edit_after_marking_updates_the_row_instead_of_duplicating_it() {
        let dir = tempdir().unwrap();
        let db = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        let path = vault.path().join("reminders.md");
        std::fs::write(&path, "- [ ] Pay tax — due 2026-04-10\n").unwrap();

        let access = VaultAccess::new();
        sync_vault_to_db(&db, vault.path(), ctx(&access, true)).unwrap();
        let id = reminders::list_all(&db).unwrap()[0].id;

        // Tick the box in Obsidian, keeping the marker as the editor would.
        let marked = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, marked.replace("- [ ]", "- [x]")).unwrap();
        sync_vault_to_db(&db, vault.path(), ctx(&access, true)).unwrap();

        let rows = reminders::list_all(&db).unwrap();
        assert_eq!(rows.len(), 1, "the edit must not create a second row: {rows:?}");
        assert_eq!(rows[0].id, id);
        assert!(rows[0].done);
    }

    #[test]
    fn write_back_never_touches_the_worklog() {
        let dir = tempdir().unwrap();
        let db = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        let path = vault.path().join("worklog.md");
        let before = "## 2026-03-30\n- Shipped the storage layer\n";
        std::fs::write(&path, before).unwrap();

        let access = VaultAccess::new();
        let s = sync_vault_to_db(&db, vault.path(), ctx(&access, true)).unwrap();

        assert_eq!(s.worklog_entries, 1, "still imported");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "worklog.md is read-only for Mervyn"
        );
    }

    #[test]
    fn a_file_that_changes_mid_sync_is_left_for_the_next_pass() {
        let dir = tempdir().unwrap();
        let db = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        let path = vault.path().join("reminders.md");
        std::fs::write(&path, "- [ ] One\n").unwrap();

        // Stand in for Obsidian flushing between the parse and the write.
        let access = VaultAccess::new();
        let raw = std::fs::read_to_string(&path).unwrap();
        let found = md::find_reminders(&raw, london());
        let mut edits = marker_edits(&found, |r| r.id);
        std::fs::write(&path, "- [ ] One\n- [ ] Two typed while syncing\n").unwrap();

        let stale = splice(&raw, &mut edits);
        assert!(!stale.contains("Two typed"), "the stale render drops the new line");

        // The real path re-reads before writing, so the typed line survives.
        let s = sync_vault_to_db(&db, vault.path(), ctx(&access, true)).unwrap();
        assert_eq!(s.markers_added, 2);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("Two typed while syncing"));
    }
}
