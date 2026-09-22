//! Parse Obsidian vault Markdown, reconcile it with redb, and write the agreed result back.
//!
//! The Markdown → redb direction always runs. The redb → Markdown direction is gated on
//! [`WriteBackPolicy`]; with it off this is exactly the import it has always been. See
//! `docs/two-way-vault-sync.md`.

use std::path::Path;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use redb::Database;

use super::git::{self, PullOutcome};
use super::md;
use super::reconcile::{self, VaultRow};
use super::write::{apply_edits, VaultAccess};
use crate::config::VaultGitSection;
use crate::storage::{vault_state, worklog, Event, Reminder, TodoItem};

/// Files Mervyn may write to. `worklog.md` is deliberately absent: it is a symlink into the
/// worklog clone, and modifying that working tree would break `git pull --ff-only`.
const MANAGED_FILES: [&str; 3] = ["reminders.md", "events.md", "todos.md"];

#[derive(Debug, Default, Clone)]
pub struct SyncStats {
    pub reminders: usize,
    pub events: usize,
    pub todos: usize,
    pub worklog_entries: usize,
    /// Items that had no id marker and were given one on this pass.
    pub markers_added: usize,
    /// Lines rewritten because the database moved and the file had not.
    pub rows_written_to_file: usize,
    /// Rows materialised into the vault for the first time — typically added from chat.
    pub rows_appended: usize,
    /// Lines removed because the row was deleted from the database.
    pub lines_removed: usize,
    /// Rows deleted because their line was removed from the file.
    pub rows_deleted: usize,
    /// The vault has diverged from its remote and a rebase could not resolve it. Write-back is
    /// paused until a person settles it, and this cycle wrote nothing.
    pub git_conflict: bool,
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

/// Reconcile one managed file, writing it if the policy allows and it actually changed.
fn sync_one<T: VaultRow>(
    db: &Database,
    vault_path: &Path,
    ctx: SyncContext<'_>,
    now: DateTime<Utc>,
    backed_up: &mut bool,
    stats: &mut SyncStats,
    count: impl Fn(&mut SyncStats, usize),
) -> anyhow::Result<()> {
    let path = vault_path.join(T::FILE);
    let raw = read_if_present(&path)?;

    if !ctx.policy.enabled {
        // Import only. No snapshots are recorded, so turning write-back on later starts from a
        // clean bootstrap rather than from stale claims about what the two sides agreed.
        if let Some(raw) = raw.as_deref() {
            count(stats, reconcile::import_only::<T>(db, raw, ctx.tz, now)?);
        }
        return Ok(());
    }

    let outcome = reconcile::reconcile_file::<T>(db, raw.as_deref(), ctx.tz, now)?;
    count(stats, outcome.changes.imported);
    stats.rows_deleted += outcome.changes.rows_deleted;

    if outcome.changes.touches_file() {
        if !*backed_up && ctx.policy.backup_before_first_write {
            match ctx.access.backup_once(vault_path, &MANAGED_FILES, now) {
                Ok(Some(dir)) => {
                    tracing::info!(dir = %dir.display(), "vault backed up before first write-back")
                }
                Ok(None) => {}
                // A write-back that cannot be undone is not worth the convenience.
                Err(e) => anyhow::bail!("vault backup failed, refusing to write: {e}"),
            }
            *backed_up = true;
        }

        let base = raw.clone().unwrap_or_default();
        let mut edits = outcome.edits;
        let updated = apply_edits(&base, &mut edits);

        // Optimistic concurrency: if Obsidian flushed a buffer while we were reconciling, the
        // offsets we measured no longer describe this file. Skip it; the next tick re-reads.
        if read_if_present(&path)? != raw {
            tracing::warn!(path = %path.display(), "vault changed during sync; write deferred");
            return Ok(());
        }

        ctx.access.write_file(&path, &updated)?;
        stats.markers_added += outcome.changes.markers_added;
        stats.rows_written_to_file += outcome.changes.rows_written_to_file;
        stats.rows_appended += outcome.changes.rows_appended;
        stats.lines_removed += outcome.changes.lines_removed;
        tracing::info!(
            path = %path.display(),
            markers = outcome.changes.markers_added,
            rewritten = outcome.changes.rows_written_to_file,
            appended = outcome.changes.rows_appended,
            removed = outcome.changes.lines_removed,
            "vault file updated"
        );
    }

    // Only once the bytes are on disk does the snapshot describe something that happened.
    for (id, rendered) in outcome.snapshots {
        vault_state::put(db, id, T::FILE, &rendered).map_err(|e| anyhow::anyhow!(e))?;
    }
    for id in outcome.tombstones_cleared {
        vault_state::take_tombstone(db, id, T::FILE).map_err(|e| anyhow::anyhow!(e))?;
    }

    Ok(())
}

/// One git-backed cycle: bring in what other devices wrote, reconcile, then send ours out.
///
/// Holds the vault lock from the pull through to the push. Anything less and a `git pull` could
/// land new bytes between a merge measuring a file's offsets and the write that uses them.
///
/// A rebase conflict stops the cycle before it writes. Mervyn's own commits are mostly
/// reproducible from the database, but not all of them are — a removal consumes a tombstone —
/// and piling more automated commits on top of a divergence only makes it harder to unpick.
pub fn sync_vault_with_git(
    db: &Database,
    vault_path: &Path,
    ctx: SyncContext<'_>,
    git_cfg: &VaultGitSection,
) -> anyhow::Result<SyncStats> {
    if !git::is_repo(vault_path) {
        anyhow::bail!(
            "[vault_git] is enabled but {} is not a git repository; clone the vault there first              (see docs/two-way-vault-sync.md)",
            vault_path.display()
        );
    }

    let _guard = ctx.access.lock();

    // Commit whatever is already on disk before rebasing, which needs a clean tree. This is the
    // ordinary case, not a recovery: the file watcher reacts to a write within milliseconds and
    // syncs it to the database long before this five-minute cycle comes round.
    if git::commit_all(vault_path, git_cfg, "mervyn: vault write-back")? {
        tracing::debug!("committed vault changes written between cycles");
    }

    if git::pull_rebase(vault_path, git_cfg)? == PullOutcome::Conflicted {
        // pull_rebase has already logged what happened and why writes are paused.
        return Ok(SyncStats {
            git_conflict: true,
            ..SyncStats::default()
        });
    }

    let stats = reconcile_all(db, vault_path, ctx)?;

    git::commit_all(vault_path, git_cfg, "mervyn: vault write-back")?;

    // Push on *anything* unpushed, not just on what this cycle happened to commit. Gating the
    // push on the commit above meant that work committed before the rebase — which is most of it,
    // since the watcher writes first — was committed locally and never sent, silently, for days.
    if git::has_unpushed(vault_path, git_cfg) {
        git::push(vault_path, git_cfg)?;
        tracing::info!(?stats, "vault changes pushed");
    }

    Ok(stats)
}

/// Reconcile the managed files with the database, then import the worklog.
///
/// Takes the vault lock for the whole cycle: read, merge and write have to be one operation, or a
/// concurrent sync could write edits measured against a file that has already moved.
pub fn sync_vault_to_db(
    db: &Database,
    vault_path: &Path,
    ctx: SyncContext<'_>,
) -> anyhow::Result<SyncStats> {
    let _guard = ctx.access.lock();
    reconcile_all(db, vault_path, ctx)
}

/// The cycle itself. The caller must already hold the vault lock.
fn reconcile_all(
    db: &Database,
    vault_path: &Path,
    ctx: SyncContext<'_>,
) -> anyhow::Result<SyncStats> {
    let now = Utc::now();
    let mut stats = SyncStats::default();
    let mut backed_up = false;

    sync_one::<Reminder>(db, vault_path, ctx, now, &mut backed_up, &mut stats, |s, n| {
        s.reminders += n
    })?;
    sync_one::<Event>(db, vault_path, ctx, now, &mut backed_up, &mut stats, |s, n| {
        s.events += n
    })?;
    sync_one::<TodoItem>(db, vault_path, ctx, now, &mut backed_up, &mut stats, |s, n| {
        s.todos += n
    })?;

    // The worklog is read-only for Mervyn, so it stays a plain import.
    if let Some(raw) = read_if_present(&vault_path.join("worklog.md"))? {
        for w in md::parse_worklog(&raw, ctx.tz) {
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
    use crate::storage::{events, reminders, todos};
    use crate::vault::marker;
    use chrono::Duration;
    use tempfile::{tempdir, TempDir};

    fn london() -> Tz {
        "Europe/London".parse().unwrap()
    }

    struct Fixture {
        db: Database,
        vault: TempDir,
        access: VaultAccess,
        _dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempdir().unwrap();
            let db = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
            Self {
                db,
                vault: tempdir().unwrap(),
                access: VaultAccess::new(),
                _dir: dir,
            }
        }

        fn ctx(&self, enabled: bool) -> SyncContext<'_> {
            SyncContext {
                access: &self.access,
                policy: WriteBackPolicy {
                    enabled,
                    backup_before_first_write: false,
                },
                tz: london(),
            }
        }

        fn sync(&self) -> SyncStats {
            sync_vault_to_db(&self.db, self.vault.path(), self.ctx(true)).unwrap()
        }

        fn import_only(&self) -> SyncStats {
            sync_vault_to_db(&self.db, self.vault.path(), self.ctx(false)).unwrap()
        }

        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.vault.path().join(name), body).unwrap();
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.vault.path().join(name)).unwrap()
        }

        fn exists(&self, name: &str) -> bool {
            self.vault.path().join(name).exists()
        }
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    // ---- import direction, unchanged ----------------------------------------------------------

    #[test]
    fn import_only_reads_the_file_and_never_writes_it() {
        let f = Fixture::new();
        let before = "- [ ] Test — due 2026-05-01\n";
        f.write("reminders.md", before);

        let s = f.import_only();

        assert_eq!(s.reminders, 1);
        assert_eq!(reminders::list_all(&f.db).unwrap()[0].body, "Test");
        assert_eq!(f.read("reminders.md"), before, "read-only means read-only");
        assert_eq!(s.markers_added + s.rows_appended, 0);
    }

    #[test]
    fn the_worklog_is_imported_but_never_written() {
        let f = Fixture::new();
        let before = "## 2026-03-30\n- Shipped the storage layer\n";
        f.write("worklog.md", before);

        let s = f.sync();

        assert_eq!(s.worklog_entries, 1);
        assert_eq!(f.read("worklog.md"), before);
    }

    // ---- markers ------------------------------------------------------------------------------

    #[test]
    fn write_back_marks_every_item_once_and_then_stops() {
        let f = Fixture::new();
        f.write("reminders.md", "- [ ] One — due 2026-05-01\n- [x] Two\n");
        f.write("events.md", "## 2026-04-05 — Gig\nDoors 7pm\n");

        let first = f.sync();
        assert_eq!(first.markers_added, 3, "two reminders and one event");

        let second = f.sync();
        assert_eq!(second.markers_added, 0, "second pass has nothing to add");

        let md = f.read("reminders.md");
        for r in reminders::list_all(&f.db).unwrap() {
            assert!(md.contains(&marker::render(r.id)), "row {} unmarked in:\n{md}", r.id);
        }
    }

    #[test]
    fn an_edit_after_marking_updates_the_row_instead_of_duplicating_it() {
        let f = Fixture::new();
        f.write("reminders.md", "- [ ] Pay tax — due 2026-04-10\n");
        f.sync();
        let id = reminders::list_all(&f.db).unwrap()[0].id;

        let marked = f.read("reminders.md");
        f.write("reminders.md", &marked.replace("- [ ]", "- [x]"));
        f.sync();

        let rows = reminders::list_all(&f.db).unwrap();
        assert_eq!(rows.len(), 1, "the edit must not create a second row: {rows:?}");
        assert_eq!(rows[0].id, id);
        assert!(rows[0].done);
    }

    // ---- database → file ----------------------------------------------------------------------

    #[test]
    fn a_reminder_added_from_chat_appears_in_the_vault() {
        let f = Fixture::new();
        reminders::put(
            &f.db,
            &Reminder {
                id: 1,
                body: "Call the accountant".into(),
                due: utc("2026-11-15T09:00:00Z"),
                recurrence: None,
                done: false,
            },
        )
        .unwrap();

        let s = f.sync();

        assert_eq!(s.rows_appended, 1);
        assert_eq!(
            f.read("reminders.md"),
            "# Reminders\n\n- [ ] Call the accountant — due 2026-11-15 09:00 <!--mv:1-->\n"
        );
        assert_eq!(f.sync().rows_appended, 0, "appended once, not every tick");
    }

    #[test]
    fn an_event_added_from_chat_appears_as_its_own_section() {
        let f = Fixture::new();
        f.write("events.md", "## 2026-04-05 — Gig\n<!--mv:ff-->\nDoors 7pm\n");
        f.sync();

        events::put(
            &f.db,
            &Event {
                id: 2,
                title: "Hospital appointment".into(),
                description: Some("Ref AB12345.".into()),
                start: utc("2026-09-23T13:30:00Z"),
                end: None,
                tags: vec![],
            },
        )
        .unwrap();
        let s = f.sync();

        assert_eq!(s.rows_appended, 1);
        let md = f.read("events.md");
        assert!(md.starts_with("## 2026-04-05 — Gig\n"), "existing section kept: {md}");
        assert!(
            md.ends_with("\n## 2026-09-23 14:30 — Hospital appointment\n<!--mv:2-->\nRef AB12345.\n"),
            "new section appended with a blank line before it: {md:?}"
        );
    }

    #[test]
    fn todos_get_their_own_file_created_for_them() {
        let f = Fixture::new();
        assert!(!f.exists("todos.md"));
        todos::put(
            &f.db,
            &TodoItem {
                id: 1,
                body: "Call the plumber".into(),
                created_at: utc("2026-09-16T10:00:00Z"),
                done: false,
            },
        )
        .unwrap();

        f.sync();

        assert_eq!(f.read("todos.md"), "# Todos\n\n- [ ] Call the plumber <!--mv:1-->\n");
    }

    #[test]
    fn a_fired_reminder_ticks_its_box_without_reflowing_the_line() {
        let f = Fixture::new();
        f.write(
            "reminders.md",
            "- [ ] Pay the **tax** bill — due 2026-11-15 09:00 <!--mv:5-->\n",
        );
        f.sync();

        // What the reminder sweep does when a non-recurring reminder comes due.
        let mut r = reminders::list_all(&f.db).unwrap().remove(0);
        r.done = true;
        reminders::put(&f.db, &r).unwrap();

        let s = f.sync();

        assert_eq!(s.rows_written_to_file, 1);
        assert_eq!(
            f.read("reminders.md"),
            "- [x] Pay the **tax** bill — due 2026-11-15 09:00 <!--mv:5-->\n",
            "only the checkbox character may change"
        );
    }

    #[test]
    fn a_fired_reminder_is_not_resurrected_by_the_next_sync() {
        let f = Fixture::new();
        f.write("reminders.md", "- [ ] Pay tax — due 2026-11-15 09:00 <!--mv:5-->\n");
        f.sync();

        let mut r = reminders::list_all(&f.db).unwrap().remove(0);
        r.done = true;
        reminders::put(&f.db, &r).unwrap();

        for _ in 0..3 {
            f.sync();
            assert!(
                reminders::list_all(&f.db).unwrap()[0].done,
                "the file must not un-fire it"
            );
        }
    }

    // ---- file → database ----------------------------------------------------------------------

    #[test]
    fn unticking_in_obsidian_reopens_the_row() {
        let f = Fixture::new();
        f.write("todos.md", "- [ ] Call the plumber <!--mv:3-->\n");
        f.sync();
        todos::mark_done(&f.db, 3).unwrap();
        f.sync();
        assert_eq!(f.read("todos.md"), "- [x] Call the plumber <!--mv:3-->\n");

        f.write("todos.md", "- [ ] Call the plumber <!--mv:3-->\n");
        f.sync();

        assert!(!todos::get(&f.db, 3).unwrap().unwrap().done, "the user reopened it");
    }

    #[test]
    fn an_edit_and_a_firing_at_once_keep_one_field_each() {
        let f = Fixture::new();
        f.write("reminders.md", "- [ ] Pay tax — due 2026-11-15 09:00 <!--mv:5-->\n");
        f.sync();

        let mut r = reminders::list_all(&f.db).unwrap().remove(0);
        r.done = true;
        reminders::put(&f.db, &r).unwrap();
        // Meanwhile the user rewords the line, still unticked.
        f.write(
            "reminders.md",
            "- [ ] Pay the tax bill — due 2026-11-15 09:00 <!--mv:5-->\n",
        );

        f.sync();

        let row = reminders::list_all(&f.db).unwrap().remove(0);
        assert_eq!(row.body, "Pay the tax bill", "wording is the user's");
        assert!(row.done, "done is the scheduler's");
        assert_eq!(
            f.read("reminders.md"),
            "- [x] Pay the tax bill — due 2026-11-15 09:00 <!--mv:5-->\n"
        );
    }

    #[test]
    fn removing_a_line_deletes_the_row() {
        let f = Fixture::new();
        f.write("reminders.md", "- [ ] One <!--mv:1-->\n- [ ] Two <!--mv:2-->\n");
        f.sync();
        assert_eq!(reminders::list_all(&f.db).unwrap().len(), 2);

        f.write("reminders.md", "- [ ] One <!--mv:1-->\n");
        let s = f.sync();

        assert_eq!(s.rows_deleted, 1);
        let rows = reminders::list_all(&f.db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 1);
        assert_eq!(f.read("reminders.md"), "- [ ] One <!--mv:1-->\n", "not re-appended");
    }

    #[test]
    fn a_missing_file_does_not_delete_the_rows_it_used_to_hold() {
        let f = Fixture::new();
        f.write("reminders.md", "- [ ] One <!--mv:1-->\n");
        f.sync();

        // A moved vault or an unmounted volume, not an edit.
        std::fs::remove_file(f.vault.path().join("reminders.md")).unwrap();
        let s = f.sync();

        assert_eq!(s.rows_deleted, 0);
        assert_eq!(reminders::list_all(&f.db).unwrap().len(), 1);
    }

    // ---- deletion from chat -------------------------------------------------------------------

    #[test]
    fn a_tombstoned_row_loses_its_section() {
        let f = Fixture::new();
        f.write(
            "events.md",
            "## 2026-04-05 — Gig\n<!--mv:1-->\nDoors 7pm\n\n## 2026-04-06 — Dentist\n<!--mv:2-->\n",
        );
        f.sync();

        // What `remove_event` does.
        events::delete(&f.db, 1).unwrap();
        vault_state::tombstone(&f.db, 1, "events.md").unwrap();

        let s = f.sync();

        assert_eq!(s.lines_removed, 1);
        assert_eq!(f.read("events.md"), "## 2026-04-06 — Dentist\n<!--mv:2-->\n");
        assert_eq!(f.sync().lines_removed, 0, "the tombstone is consumed");
    }

    #[test]
    fn a_tombstone_survives_a_cycle_that_did_not_write() {
        let f = Fixture::new();
        f.write("events.md", "## 2026-04-05 — Gig\n<!--mv:1-->\nDoors 7pm\n");
        f.sync();
        events::delete(&f.db, 1).unwrap();
        vault_state::tombstone(&f.db, 1, "events.md").unwrap();

        // Write-back off: nothing is removed, and the instruction must not be thrown away.
        f.import_only();
        assert!(f.read("events.md").contains("Gig"));
        assert!(vault_state::has_tombstone(&f.db, 1, "events.md").unwrap());

        let s = f.sync();
        assert_eq!(s.lines_removed, 1, "the deferred removal still happens");
        assert!(!vault_state::has_tombstone(&f.db, 1, "events.md").unwrap());
    }

    // ---- git-backed cycle ---------------------------------------------------------------------

    /// A bare repo standing in for GitHub, and a second clone standing in for the phone.
    fn git_backed(f: &Fixture) -> (TempDir, TempDir, VaultGitSection) {
        let cfg = VaultGitSection {
            enabled: true,
            remote: "origin".into(),
            branch: "main".into(),
            sync_cron: "0 */5 * * * *".into(),
            author_name: "Mervyn".into(),
            author_email: "mervyn@localhost".into(),
        };
        let remote = tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .current_dir(remote.path())
            .output()
            .unwrap();

        let g = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(f.vault.path())
                .output()
                .unwrap()
        };
        g(&["init", "--initial-branch=main"]);
        g(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
        f.write("README.md", "# Vault\n");
        crate::vault::git::commit_all(f.vault.path(), &cfg, "seed").unwrap();
        crate::vault::git::push(f.vault.path(), &cfg).unwrap();

        let phone = tempdir().unwrap();
        std::process::Command::new("git")
            .args(["clone", remote.path().to_str().unwrap(), "."])
            .current_dir(phone.path())
            .output()
            .unwrap();
        (remote, phone, cfg)
    }

    #[test]
    fn a_git_cycle_carries_the_database_out_and_an_edit_back_in() {
        let f = Fixture::new();
        let (_remote, phone, git_cfg) = git_backed(&f);

        todos::put(
            &f.db,
            &TodoItem {
                id: 1,
                body: "Call the plumber".into(),
                created_at: utc("2026-09-16T10:00:00Z"),
                done: false,
            },
        )
        .unwrap();

        sync_vault_with_git(&f.db, f.vault.path(), f.ctx(true), &git_cfg).unwrap();

        // The phone sees the todo Mervyn materialised.
        std::process::Command::new("git")
            .args(["pull", "--rebase", "origin", "main"])
            .current_dir(phone.path())
            .output()
            .unwrap();
        let on_phone = std::fs::read_to_string(phone.path().join("todos.md")).unwrap();
        assert!(on_phone.contains("Call the plumber"), "not on the phone: {on_phone}");

        // Tick it there and push.
        std::fs::write(phone.path().join("todos.md"), on_phone.replace("- [ ]", "- [x]")).unwrap();
        crate::vault::git::commit_all(phone.path(), &git_cfg, "tick on the phone").unwrap();
        crate::vault::git::push(phone.path(), &git_cfg).unwrap();

        sync_vault_with_git(&f.db, f.vault.path(), f.ctx(true), &git_cfg).unwrap();

        assert!(
            todos::get(&f.db, 1).unwrap().unwrap().done,
            "the phone's tick reached the database"
        );
    }

    #[test]
    fn work_written_between_cycles_still_reaches_the_remote() {
        // The regression that let the vault stop publishing for five days while looking healthy.
        // The watcher writes a file within milliseconds of a change, so by the time the git cycle
        // runs the work is already on disk. Gating the push on what *this* cycle committed meant
        // it was committed locally and never sent — and nothing said so.
        let f = Fixture::new();
        let (_remote, phone, git_cfg) = git_backed(&f);

        // Stand in for the watcher having already written and committed a change.
        f.write("todos.md", "- [ ] written by the watcher <!--mv:1-->\n");
        crate::vault::git::commit_all(f.vault.path(), &git_cfg, "mervyn: vault write-back").unwrap();
        assert!(crate::vault::git::has_unpushed(f.vault.path(), &git_cfg));

        // This cycle finds nothing of its own to write, and must still push.
        sync_vault_with_git(&f.db, f.vault.path(), f.ctx(true), &git_cfg).unwrap();

        assert!(!crate::vault::git::has_unpushed(f.vault.path(), &git_cfg), "nothing left behind");
        std::process::Command::new("git")
            .args(["pull", "--rebase", "origin", "main"])
            .current_dir(phone.path())
            .output()
            .unwrap();
        let on_phone = std::fs::read_to_string(phone.path().join("todos.md")).unwrap();
        assert!(on_phone.contains("written by the watcher"), "reached the remote: {on_phone}");
    }

    #[test]
    fn a_diverged_vault_pauses_instead_of_writing() {
        let f = Fixture::new();
        let (_remote, phone, git_cfg) = git_backed(&f);
        f.write("todos.md", "- [ ] one <!--mv:1-->\n- [ ] two <!--mv:2-->\n");
        sync_vault_with_git(&f.db, f.vault.path(), f.ctx(true), &git_cfg).unwrap();

        // The phone rewords one line and publishes it.
        std::process::Command::new("git")
            .args(["pull", "--rebase", "origin", "main"])
            .current_dir(phone.path())
            .output()
            .unwrap();
        std::fs::write(
            phone.path().join("todos.md"),
            "- [ ] one, reworded <!--mv:1-->\n- [ ] two <!--mv:2-->\n",
        )
        .unwrap();
        crate::vault::git::commit_all(phone.path(), &git_cfg, "reword").unwrap();
        crate::vault::git::push(phone.path(), &git_cfg).unwrap();

        // Meanwhile Mervyn writes and commits the line next to it without having seen that —
        // a cycle whose push failed, or a watcher write between cycles.
        f.write("todos.md", "- [ ] one <!--mv:1-->\n- [x] two <!--mv:2-->\n");
        crate::vault::git::commit_all(f.vault.path(), &git_cfg, "mervyn: vault write-back").unwrap();

        let stats = sync_vault_with_git(&f.db, f.vault.path(), f.ctx(true), &git_cfg).unwrap();

        assert!(stats.git_conflict, "a divergence it cannot rebase must be reported");
        assert_eq!(stats.rows_written_to_file, 0, "and nothing written on top of it");
        assert!(
            crate::vault::git::has_unpushed(f.vault.path(), &git_cfg),
            "its own commit is kept, not discarded"
        );
    }

    // ---- stability ----------------------------------------------------------------------------

    #[test]
    fn a_settled_vault_is_not_rewritten() {
        let f = Fixture::new();
        f.write("reminders.md", "- [ ] One — due 2026-11-15 09:00\n");
        f.write("events.md", "## 2026-04-05 — Gig\nDoors 7pm\n");
        f.sync();

        let before = (f.read("reminders.md"), f.read("events.md"));
        let s = f.sync();

        assert_eq!(
            s.markers_added + s.rows_appended + s.rows_written_to_file + s.lines_removed,
            0,
            "a settled vault produces no edits"
        );
        assert_eq!((f.read("reminders.md"), f.read("events.md")), before);
    }

    #[test]
    fn everything_the_parser_does_not_model_survives_a_write() {
        let f = Fixture::new();
        let raw = "---\ntitle: Reminders\n---\n\n# Reminders\n\nProse Mervyn knows nothing about.\n\n> [!note] a callout\n\n- [ ] Pay tax — due 2026-11-15 09:00\n\n*A [link](https://example.com) at the end.*\n";
        f.write("reminders.md", raw);

        f.sync();

        let after = f.read("reminders.md");
        let id = reminders::list_all(&f.db).unwrap()[0].id;
        assert_eq!(
            after,
            raw.replace(
                "09:00\n",
                &format!("09:00 {}\n", marker::render(id))
            ),
            "only the reminder line may change"
        );
    }

    #[test]
    fn a_file_that_changes_mid_sync_is_left_for_the_next_pass() {
        let f = Fixture::new();
        reminders::put(
            &f.db,
            &Reminder {
                id: 1,
                body: "From chat".into(),
                due: utc("2026-11-15T09:00:00Z") + Duration::days(1),
                recurrence: None,
                done: false,
            },
        )
        .unwrap();
        f.write("reminders.md", "- [ ] Typed by hand\n");

        // Sync reads, then the editor flushes before the write lands.
        let raw = f.read("reminders.md");
        assert!(!raw.contains("From chat"));

        let s = f.sync();
        assert!(s.rows_appended > 0);
        let after = f.read("reminders.md");
        assert!(after.contains("Typed by hand"), "the hand-typed line survives: {after}");
        assert!(after.contains("From chat"), "and the chat row lands: {after}");
    }

    #[test]
    fn rows_of_different_kinds_sharing_an_id_do_not_collide() {
        // Ids are unique per table, not across them: an event and a todo are both id 1 on the
        // deployed database. Keying the snapshot by id alone made the todo find the event's
        // snapshot, conclude its line had been deleted, and never materialise.
        let f = Fixture::new();
        events::put(
            &f.db,
            &Event {
                id: 1,
                title: "Dentist".into(),
                description: None,
                start: utc("2026-10-12T10:05:00Z"),
                end: None,
                tags: vec![],
            },
        )
        .unwrap();
        todos::put(
            &f.db,
            &TodoItem {
                id: 1,
                body: "Call the plumber".into(),
                created_at: utc("2026-09-16T10:00:00Z"),
                done: false,
            },
        )
        .unwrap();

        f.sync();

        assert!(f.exists("events.md"), "the event should be written");
        assert!(f.exists("todos.md"), "and so should the todo");
        assert_eq!(todos::list_all(&f.db).unwrap().len(), 1, "and not be deleted");
    }
}
