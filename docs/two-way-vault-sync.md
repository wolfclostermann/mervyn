# Two-way vault sync

*Design and staging plan for making the Obsidian vault a **view** as well as an input.
Written 2026-09-16. Decisions at the bottom are settled; the phases are not yet built.*

---

## Where we are

`src/vault/` is one-way, Markdown → redb, and that is baked in at three levels:

| Piece | Behaviour |
|---|---|
| `src/vault/sync.rs` | Reads `reminders.md`, `events.md`, `worklog.md`; upserts every parsed row. No deletes, no db→md path. |
| `src/vault/md.rs` | Parsers are lossy and span-free — they collect plain text and discard offsets, formatting and ordering. |
| `src/vault/md.rs` (`stable_vault_row_id`) | Row identity is `FNV(content)`. |
| `src/vault/watcher.rs` | Debounced notify → full re-sync. |

Three consequences, all live today:

1. **Editing a synced line orphans a row.** The reminder id hashes `body|due|done`, so ticking a
   checkbox in Obsidian mints a *new* id. The old unchecked row stays in redb for ever and keeps
   firing in `run_reminder_check`.
2. **Deleting a line does nothing.** Nothing ever calls `events::delete` / `reminders::delete`
   from sync.
3. **Chat-created rows are invisible in Obsidian** — `add_event`, `add_reminder` and `add_note`
   write only to redb.

This is item 4 in `code-audit-fix-list.md` and the "Obsidian is an input, never a view" paragraph
in `handoff-2026-09-16.md`. Two-way sync is really two jobs: give items a **stable identity in the
file**, then add the **db→md direction**. The first is most of the value and fixes bugs that exist
right now.

---

## Design

### 1. Identity: an explicit marker in the file

Nothing else works. Content hashing cannot distinguish an edit from a new item, by construction.

Each item carries an HTML comment holding its row id, at the end of its anchor line:

```markdown
- [ ] Pay tax — due 2026-04-10 — recurs yearly <!--mv:8fa1c3d2-->

## 2026-04-05 — Gig at Tap
<!--mv:1a2b3c4d-->
Doors 7pm
```

Hidden in Obsidian's reading view, dimmed in live preview. Usefully, **the current parsers already
ignore it**: `collect_plain_until` drops everything that is not `Text` / `Code` / a break, so
markers are backward-compatible and the back-fill can ship before anything else changes.

Obsidian block ids (`^mv-8fa1c3d2`) were the alternative — more native, but awkward on
heading-delimited event sections, and they lose the "old parser ignores them" property.

**Migration is free.** On the first back-fill, parse exactly as today (hash ids), then write each
row's *existing* hash id into the file as its marker. The database does not move. After that ids
come only from `next_id`; the hash survives as a bootstrap only. Run the back-fill once immediately
after deploy — a hand edit made between the last sync and the back-fill still duplicates, exactly
as it does today.

### 2. Surgical edits, not regeneration

Do **not** render files from the database. The parser drops `**bold**`, callouts, links,
interleaved prose, blank-line layout and front matter; a db→md render would quietly destroy the
vault. Every write is a minimal text edit against the original bytes: insert a line, replace a
line, flip one `[ ]` → `[x]`, delete a line.

That needs source spans, which means two changes in `md.rs`:

- Parse with `Parser::into_offset_iter()` so each item carries a `Range<usize>`.
- **Fix `strip_yaml_front_matter`**, which returns `lines[i + 1..].join("\n")`. That both loses
  CRLF and destroys offset correspondence with the raw file. It must return a byte offset into
  `raw` instead, so spans map back to real file positions.

### 3. Three-way merge, not last-writer-wins

To tell "the human edited the file" from "the scheduler changed the row" you need the last-synced
state. A new redb table alongside the seven in `storage/db.rs`:

```rust
pub const VAULT_SYNC_STATE: TableDefinition<u64, &[u8]> = TableDefinition::new("vault_sync_state");
// { file: String, snapshot: <rendered form at last sync>, seen_at: i64 }
```

Then, per item, compare md / db / snapshot field by field:

| Situation | Resolution |
|---|---|
| md differs, db same | apply md to db |
| db differs, md same | rewrite the line |
| both differ | field-level: **md wins** for `body` / `title` / `description`, **db wins** for `done` / `due` (the scheduler owns those — `run_reminder_check` advances them). Log the conflict. |
| in md with a marker, absent from db | deleted via chat → remove the line |
| in db, absent from md, present in snapshot | deleted in Obsidian → delete the row |
| in md, no marker | new → insert, assign id, write the marker back |

Deletes need **tombstones**. If a row is deleted and the following file write fails, the line comes
back and resurrects the row on the next tick. A tombstoned id must be dropped on parse.

### 4. Loop and concurrency control

- **Self-write echo.** After writing, record `(path, content_hash)`; the watcher skips a file whose
  current hash matches the last thing Mervyn wrote. Sync is idempotent so a loop would not corrupt
  anything, but it would churn every 350 ms.
- **A vault lock.** `sync_vault_to_db` is called from three places with no mutual exclusion today —
  cron, the watcher thread and the git-pull job. Harmless for idempotent upserts; not harmless once
  it writes.
- **Optimistic re-check.** Re-read the file and compare against the hash seen at parse time
  immediately before writing; if it moved (Obsidian flushed a buffer), redo the merge. Write via a
  temp file and `rename` within the same directory.

### 5. Worklog stays one-way

`data/vault/worklog.md` is a symlink into a git clone (`/worklog/worklog.md` in-container). Writing
into it puts unstaged changes in that working tree, and `git pull --ff-only` in
`run_worklog_git_pull` then **fails**. Two-way worklog means committing and pushing, which is a
separate project. The pull is disabled at the moment, so the breakage is latent rather than
visible — which is exactly why it is written down here. Chat `log_work` entries stay database-only.

### 6. `todos.md` is the easy win

Todos have no vault file at all and no legacy parser. A fresh `todos.md` of `- [ ] …` lines is the
most Obsidian-native surface here, and both directions are obvious: tick in Obsidian →
`todos::mark_done`; `complete_todo` in chat → the checkbox flips in the file.

### 7. The assembler will double-count

`build_query_context` feeds Claude *both* the database rows and the raw `events.md` /
`worklog.md` text. Once the database is mirrored into the vault those are the same items — token
bloat, and Claude will report duplicates. Once phase 2 is verified, drop the raw `events.md` block
from the query context. This is part of the work, not a follow-up.

---

## Phases

| Phase | Content | Risk |
|---|---|---|
| **0** | Span-preserving front-matter strip; vault lock; atomic-write helper; self-write suppression; `[vault] write_back_enabled = false`; one-shot backup | None — no writes yet |
| **1** | Marker grammar, canonical renderers, `into_offset_iter` parsing, marker back-fill write. **Fixes the orphan-on-edit bug on its own.** | First writes to the vault; deploy dark, verify in logs, then enable |
| **2** | db → md: chat-created events / reminders / todos appear; `remove_event` strips lines; reminder firing ticks boxes; `todos.md` introduced | Medium |
| **3** | md → db deletes with tombstones; `vault_sync_state` three-way merge | Medium |
| **4** | Assembler dedupe; README "Obsidian vault", spec, and the handoff's "Obsidian is an input, never a view" paragraph; close audit item 4 | Low |

Module layout: `src/vault/{md.rs, render.rs, marker.rs, reconcile.rs, write.rs, watcher.rs}`, with
`sync.rs` becoming a thin `reconcile()` wrapper so callers do not all change at once.

## Tests that matter

- `parse(render(x)) == x` round-trip per type.
- Writing twice produces byte-identical output — no churn.
- A hand-edited line keeps its id.
- A line deleted in Obsidian deletes exactly one row.
- A tombstoned marker does not resurrect.
- CRLF files and front-matter files survive a write unchanged.
- A file with prose, bold and callouts between items is byte-identical outside the edited line.

## Decisions

- **Marker style**: HTML comments in the existing single files. Not one-file-per-event; not block ids.
- **Worklog**: out of scope, for the `git pull --ff-only` reason above.
- **Write-back ships disabled** (`[vault] write_back_enabled = false`) so each phase can be
  deployed dark and verified from the logs before it is allowed to touch the vault.
