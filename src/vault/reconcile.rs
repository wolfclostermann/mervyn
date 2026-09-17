//! The three-way merge between a vault file, the database, and what the two last agreed on.
//!
//! Each managed row type answers the same questions — how to find it in a file, how to render it,
//! which side owns which field when both have moved — so one generic pass drives all of them.
//!
//! The snapshot in [`crate::storage::vault_state`] is what makes the merge possible. Without it,
//! a reminder ticked in redb and clear in Markdown is ambiguous, and whichever side sync happened
//! to prefer would either resurrect a fired reminder for ever or discard the user's edit.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use redb::Database;

use super::md::{self, FoundItem};
use super::render;
use super::write::Edit;
use crate::storage::{event_notices, events, reminders, todos, vault_state};
use crate::storage::{Event, Reminder, TodoItem};

/// Everything a row type needs to take part in the merge.
pub trait VaultRow: Clone + PartialEq + Sized {
    /// File under the vault root that holds this kind of row.
    const FILE: &'static str;
    /// Written when the file has to be created from nothing.
    const HEADER: &'static str;
    /// Placed before each appended block — a blank line for sections, nothing for list items.
    const BLOCK_SEPARATOR: &'static str;

    fn id(&self) -> u64;
    fn done(&self) -> bool;

    /// Text replacing the item's anchor when the database side changed.
    fn render_anchor(&self, tz: Tz) -> String;
    /// Newline-terminated text appended when the row has no line in the file yet. Also what the
    /// snapshot stores, so it must parse back to an equal row.
    fn render_block(&self, tz: Tz) -> String;

    /// The row that results when a human's edit and the database's own change have to coexist.
    fn merge(md: &Self, db: &Self) -> Self;

    /// The file's version of the row, keeping any field the file cannot express.
    fn adopt(md: &Self, db: &Self) -> Self;

    /// The same row with `done` set. Types without the field return themselves unchanged.
    fn with_done(&self, done: bool) -> Self;

    fn find(raw: &str, tz: Tz, now: DateTime<Utc>) -> Vec<FoundItem<Self>>;

    fn load_all(db: &Database) -> anyhow::Result<Vec<Self>>;
    fn put(db: &Database, row: &Self) -> anyhow::Result<()>;
    fn delete(db: &Database, id: u64) -> anyhow::Result<()>;
}

impl VaultRow for Reminder {
    const FILE: &'static str = "reminders.md";
    const HEADER: &'static str = "# Reminders\n\n";
    const BLOCK_SEPARATOR: &'static str = "";

    fn id(&self) -> u64 {
        self.id
    }
    fn done(&self) -> bool {
        self.done
    }
    fn render_anchor(&self, tz: Tz) -> String {
        render::reminder_line(self, tz)
    }
    fn render_block(&self, tz: Tz) -> String {
        format!("{}\n", render::reminder_line(self, tz))
    }

    /// The scheduler owns `done` and `due` — it is what advances a recurring reminder and closes
    /// a fired one. Everything the user can say in a sentence comes from the file.
    fn merge(md: &Self, db: &Self) -> Self {
        Self {
            id: md.id,
            body: md.body.clone(),
            recurrence: md.recurrence.clone(),
            due: db.due,
            done: db.done,
        }
    }

    fn adopt(md: &Self, _db: &Self) -> Self {
        md.clone()
    }

    fn with_done(&self, done: bool) -> Self {
        Self { done, ..self.clone() }
    }

    fn find(raw: &str, tz: Tz, _now: DateTime<Utc>) -> Vec<FoundItem<Self>> {
        md::find_reminders(raw, tz)
    }
    fn load_all(db: &Database) -> anyhow::Result<Vec<Self>> {
        reminders::list_all(db).map_err(|e| anyhow::anyhow!(e))
    }
    fn put(db: &Database, row: &Self) -> anyhow::Result<()> {
        reminders::put(db, row).map_err(|e| anyhow::anyhow!(e))
    }
    fn delete(db: &Database, id: u64) -> anyhow::Result<()> {
        reminders::delete(db, id).map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }
}

impl VaultRow for Event {
    const FILE: &'static str = "events.md";
    const HEADER: &'static str = "# Events\n\n";
    const BLOCK_SEPARATOR: &'static str = "\n";

    fn id(&self) -> u64 {
        self.id
    }
    fn done(&self) -> bool {
        false
    }

    /// Only the heading line. An event's description is free Markdown the parser flattens, so
    /// re-rendering the section would quietly strip a link or a bold word out of the user's note.
    fn render_anchor(&self, tz: Tz) -> String {
        render::event_section(self, tz)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }
    fn render_block(&self, tz: Tz) -> String {
        render::event_section(self, tz)
    }

    /// Nothing in the running system changes an event behind the user's back, so the file wins
    /// outright. Kept explicit rather than implied, so that stops being true loudly.
    fn merge(md: &Self, _db: &Self) -> Self {
        md.clone()
    }

    fn adopt(md: &Self, _db: &Self) -> Self {
        md.clone()
    }

    fn with_done(&self, _done: bool) -> Self {
        self.clone()
    }

    fn find(raw: &str, tz: Tz, _now: DateTime<Utc>) -> Vec<FoundItem<Self>> {
        md::find_events(raw, tz)
    }
    fn load_all(db: &Database) -> anyhow::Result<Vec<Self>> {
        events::list_all(db).map_err(|e| anyhow::anyhow!(e))
    }
    fn put(db: &Database, row: &Self) -> anyhow::Result<()> {
        events::put(db, row).map_err(|e| anyhow::anyhow!(e))
    }
    fn delete(db: &Database, id: u64) -> anyhow::Result<()> {
        events::delete(db, id).map_err(|e| anyhow::anyhow!(e))?;
        if let Err(e) = event_notices::delete(db, id) {
            tracing::warn!(id, error = %e, "event_notices delete during vault reconcile");
        }
        Ok(())
    }
}

impl VaultRow for TodoItem {
    const FILE: &'static str = "todos.md";
    const HEADER: &'static str = "# Todos\n\n";
    const BLOCK_SEPARATOR: &'static str = "";

    fn id(&self) -> u64 {
        self.id
    }
    fn done(&self) -> bool {
        self.done
    }
    fn render_anchor(&self, _tz: Tz) -> String {
        render::todo_line(self)
    }
    fn render_block(&self, _tz: Tz) -> String {
        format!("{}\n", render::todo_line(self))
    }

    /// `complete_todo` sets `done` from chat; the body is the user's to word. `created_at` is not
    /// in the file at all, so it can only come from the database.
    fn merge(md: &Self, db: &Self) -> Self {
        Self {
            id: md.id,
            body: md.body.clone(),
            created_at: db.created_at,
            done: db.done,
        }
    }

    /// `created_at` is not written to the file, so the database is its only home.
    fn adopt(md: &Self, db: &Self) -> Self {
        Self {
            created_at: db.created_at,
            ..md.clone()
        }
    }

    fn with_done(&self, done: bool) -> Self {
        Self { done, ..self.clone() }
    }

    fn find(raw: &str, _tz: Tz, now: DateTime<Utc>) -> Vec<FoundItem<Self>> {
        md::find_todos(raw, now)
    }
    fn load_all(db: &Database) -> anyhow::Result<Vec<Self>> {
        todos::list_all(db).map_err(|e| anyhow::anyhow!(e))
    }
    fn put(db: &Database, row: &Self) -> anyhow::Result<()> {
        todos::put(db, row).map_err(|e| anyhow::anyhow!(e))
    }
    fn delete(db: &Database, id: u64) -> anyhow::Result<()> {
        todos::delete(db, id).map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileChanges {
    pub imported: usize,
    pub markers_added: usize,
    pub rows_written_to_file: usize,
    pub rows_appended: usize,
    pub lines_removed: usize,
    pub rows_deleted: usize,
}

impl FileChanges {
    pub fn touches_file(&self) -> bool {
        self.markers_added + self.rows_written_to_file + self.rows_appended + self.lines_removed > 0
    }
}

/// The outcome of reconciling one file: the edits to apply, and what they mean.
pub struct Reconciled {
    pub edits: Vec<Edit>,
    pub changes: FileChanges,
    /// Snapshots to record once the write has actually landed.
    pub snapshots: Vec<(u64, String)>,
}

/// Rewrite an item's text from `row`, as cheaply as the change allows.
///
/// A fired reminder differs from its line only in the checkbox, and flipping that one character
/// keeps whatever the user wrote around it — bold, a link, a trailing note. Anything else needs
/// the canonical rendering.
fn rewrite_edit<T: VaultRow>(found: &FoundItem<T>, row: &T, tz: Tz) -> Edit {
    let only_the_box =
        found.item.done() != row.done() && found.item.with_done(row.done()) == *row;
    match (only_the_box, found.checkbox_at) {
        (true, Some(at)) => Edit::replace(at..at + 1, if row.done() { "x" } else { " " }),
        _ => Edit::replace(found.anchor.clone(), row.render_anchor(tz)),
    }
}

/// Append `block` to `tail`, keeping a blank line between sections where the type wants one.
fn push_block<T: VaultRow>(base: &str, tail: &mut String, block: &str) {
    if !T::BLOCK_SEPARATOR.is_empty() {
        let tip = if tail.is_empty() { base } else { tail.as_str() };
        if !tip.is_empty() && !tip.ends_with("\n\n") {
            tail.push_str(T::BLOCK_SEPARATOR);
        }
    }
    tail.push_str(block);
}

/// Work out what one managed file and the database owe each other.
///
/// `raw` is `None` when the file does not exist yet. Nothing here touches the filesystem: the
/// caller applies [`Reconciled::edits`] and only then commits [`Reconciled::snapshots`], so a
/// failed write cannot leave behind a snapshot claiming the two sides agree.
pub fn reconcile_file<T: VaultRow>(
    db: &Database,
    raw: Option<&str>,
    tz: Tz,
    now: DateTime<Utc>,
) -> anyhow::Result<Reconciled> {
    let mut out = Reconciled {
        edits: Vec::new(),
        changes: FileChanges::default(),
        snapshots: Vec::new(),
    };

    let found = raw.map(|r| T::find(r, tz, now)).unwrap_or_default();
    let db_rows: HashMap<u64, T> = T::load_all(db)?
        .into_iter()
        .map(|r| (r.id(), r))
        .collect();
    let mut seen: HashSet<u64> = HashSet::new();

    for f in &found {
        let id = f.item.id();
        seen.insert(id);
        out.changes.imported += 1;

        // Deleted from the database by chat: take the line out and stop tracking the row.
        if vault_state::take_tombstone(db, id)? {
            out.edits.push(Edit::remove(f.span.clone()));
            vault_state::forget(db, id)?;
            out.changes.lines_removed += 1;
            continue;
        }

        if !f.had_marker {
            T::put(db, &f.item)?;
            if let Some((at, text)) = f.backfill(id) {
                out.edits.push(Edit::insert(at, text));
                out.changes.markers_added += 1;
            }
            out.snapshots.push((id, f.item.render_block(tz)));
            continue;
        }

        let snapshot = vault_state::get(db, id)?.map(|s| s.rendered);
        let Some((row, snapshot)) = db_rows.get(&id).zip(snapshot) else {
            // Either the database has never seen this row or it no longer has it, and no
            // tombstone says that was deliberate. The file is the record; take it at its word.
            T::put(db, &f.item)?;
            out.snapshots.push((id, f.item.render_block(tz)));
            continue;
        };

        // Compared as rendered text, not as rows: it is exactly the part of a row the file can
        // express, so a field the Markdown never carries cannot masquerade as an edit.
        let md_changed = f.item.render_block(tz) != snapshot;
        let db_changed = row.render_block(tz) != snapshot;

        let resolved = match (md_changed, db_changed) {
            (false, false) => continue,
            (true, false) => T::adopt(&f.item, row),
            (false, true) => row.clone(),
            (true, true) => T::merge(&f.item, row),
        };

        if resolved != *row {
            T::put(db, &resolved)?;
        }
        let rendered = resolved.render_block(tz);
        if rendered != f.item.render_block(tz) {
            out.edits.push(rewrite_edit(f, &resolved, tz));
            out.changes.rows_written_to_file += 1;
        }
        out.snapshots.push((id, rendered));
    }

    // Rows with no line in the file: either they have never been written there, or the user
    // deleted the line.
    let mut appends: Vec<&T> = Vec::new();
    let mut deletions: Vec<u64> = Vec::new();
    let mut unseen: Vec<(&u64, &T)> = db_rows.iter().filter(|(id, _)| !seen.contains(id)).collect();
    unseen.sort_by_key(|(id, _)| **id);
    for (id, row) in unseen {
        if vault_state::get(db, *id)?.is_some() {
            deletions.push(*id);
        } else {
            appends.push(row);
        }
    }

    if raw.is_none() && !deletions.is_empty() {
        // The file is gone, not edited — a moved vault or an unmounted volume. Deleting rows on
        // that evidence would turn a mount problem into data loss.
        tracing::warn!(
            file = T::FILE,
            rows = deletions.len(),
            "vault file missing; not deleting rows it used to hold"
        );
        deletions.clear();
    }

    for id in deletions {
        T::delete(db, id)?;
        vault_state::forget(db, id)?;
        out.changes.rows_deleted += 1;
    }

    if !appends.is_empty() {
        let base = raw.unwrap_or("");
        let mut tail = String::new();
        if base.is_empty() {
            tail.push_str(T::HEADER);
        } else if !base.ends_with('\n') {
            tail.push('\n');
        }
        for row in appends {
            let block = row.render_block(tz);
            push_block::<T>(base, &mut tail, &block);
            out.snapshots.push((row.id(), block));
            out.changes.rows_appended += 1;
        }
        out.edits.push(Edit::insert(base.len(), tail));
    }

    Ok(out)
}

/// Import without write-back: every item in the file is upserted and nothing is recorded, which
/// is exactly what sync did before any of this existed.
pub fn import_only<T: VaultRow>(
    db: &Database,
    raw: &str,
    tz: Tz,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let found = T::find(raw, tz, now);
    for f in &found {
        T::put(db, &f.item)?;
    }
    Ok(found.len())
}
