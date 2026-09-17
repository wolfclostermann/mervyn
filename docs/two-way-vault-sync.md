# Two-way vault sync

*Design and staging plan for making the Obsidian vault a **view** as well as an input.
Written 2026-09-16, built 2026-09-17. All four phases have landed; write-back ships disabled.*

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
row's *existing* hash id into the file as its marker. The database does not move.

**The hash is not only a bootstrap** — an earlier draft of this document said it was, and that
undersold it. Inserting a row and writing its marker back are two steps, and the second can fail:
a write error, or the file moving under the cycle, which is routine once a phone and a laptop push
to it. Deriving the id from the content makes that failure self-healing — the next cycle reads the
same unmarked line, derives the same id, and upserts the same row. A counter would allocate a new
id instead, insert a second row, and then append the first one back into the file as a row with no
line. The ugly 20-digit ids are the price of that idempotency, and after
`format_todos` stopped printing database keys they are no longer visible anywhere but the markers.

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

### 5b. …and then the worklog moved into the vault repo

*Added 2026-09-17.* The section below is why the worklog was excluded from write-back, and it held
for as long as `worklog.md` was a symlink into a separate clone that Mervyn only pulled. Once the
vault itself had to become a git repo — to reach a laptop and a phone — keeping a second repo for
one file stopped making sense. The worklog is now an ordinary file in the vault repo, `[vault_git]`
replaces `[worklog_git]`, and the sync runs in both directions, so `git pull --ff-only` is no
longer the constraint it was. Mervyn still never *writes* `worklog.md`: it is appended by the
commit hook in `contrib/mervyn-worklog-sync`, and read by the assembler.

### 5. Worklog stays one-way (historical)

`data/vault/worklog.md` is a symlink into a git clone (`/worklog/worklog.md` in-container). Writing
into it puts unstaged changes in that working tree, and `git pull --ff-only` in
`run_worklog_git_pull` then **fails**. Two-way worklog means committing and pushing, which is a
separate project. The pull is disabled at the moment, so the breakage is latent rather than
visible — which is exactly why it is written down here. Chat `log_work` entries stay database-only.

### 6. Times are local wall-clock

The grammar takes an optional `HH:MM` on a reminder's due date and `HH:MM[–HH:MM]` on an event
heading, written in `scheduler.timezone` with **no offset suffix**. A bare date means local noon,
so noon renders back as a bare date and still round-trips exactly.

Local won over UTC and over a written offset for one reason: both alternatives put arithmetic
between the user and their own notes, and the arithmetic changes with the date. To schedule 09:00
in November you would write `09:00Z`, but 09:00 tomorrow is `08:00Z` — same number on the clock,
different number in the file. A written offset is worse still, because hand-typing the offset you
are *currently* living in for a date on the other side of a changeover is a well-formed way to be
an hour early.

The offset is resolved for the date being converted, never for today, so `2026-11-15 09:00` written
in August is 09:00 GMT that morning. That rule lives in `local_time.rs`, shared with the chat path
so the two cannot drift, with the DST edges under test: the repeated hour resolves to the later
instant, the missing hour is pushed forward.

The residue: the file is not self-describing, so changing `scheduler.timezone` would silently
re-mean every stored line. Acceptable for a single-user vault pinned to `Europe/London`, and
recorded here rather than discovered later.

### 7. `todos.md` is the easy win

Todos have no vault file at all and no legacy parser. A fresh `todos.md` of `- [ ] …` lines is the
most Obsidian-native surface here, and both directions are obvious: tick in Obsidian →
`todos::mark_done`; `complete_todo` in chat → the checkbox flips in the file.

### 8. The assembler will double-count

`build_query_context` feeds Claude *both* the database rows and the raw `events.md` /
`worklog.md` text. Once the database is mirrored into the vault those are the same items — token
bloat, and Claude will report duplicates. Once phase 2 is verified, drop the raw `events.md` block
from the query context. This is part of the work, not a follow-up.

---

## Phases

| Phase | Content | Risk |
|---|---|---|
| **0** ✅ | Span-preserving front-matter strip; vault lock; atomic-write helper; self-write suppression; `[vault] write_back_enabled = false`; one-shot backup | None — no writes |
| **1** ✅ | Marker grammar, `into_offset_iter` parsing, marker back-fill write. **Fixed the orphan-on-edit bug on its own.** | First writes to the vault |
| **2** ✅ | Local wall-clock grammar; canonical renderers with round-trip tests | None on its own |
| **3** ✅ | `vault_state` snapshots; three-way merge; db → md materialisation; deletes in both directions; `todos.md` | The substantive one |
| **4** ✅ | Assembler dedupe; README, handoff and audit item 4 | Low |

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

## What shipped, and what to know about it

**Rewrites are as small as the change allows.** A fired reminder differs from its line only in the
checkbox, so that single character is replaced in place and `- [ ] Pay the **tax** bill` keeps its
bold. An event is never re-rendered wholesale — only its heading line — because the description is
free Markdown the parser flattens, and rewriting the section would strip a link out of the note.

**The re-firing bug is fixed.** It predated this work: a vault-sourced reminder that fired was set
`done` in redb, then reset to unticked by the next import, and fired again five minutes later, for
ever. It went unnoticed only because the deployed vault has no `reminders.md`. The snapshot is what
resolves it — without one, that state is genuinely ambiguous.

**Deleting the whole file deletes nothing.** A line that had a snapshot and is now gone deletes its
row, but only when the file is still there. A missing file is a moved vault or an unmounted volume.
The gap that remains: a file *truncated to empty* still reads as "everything was deleted". The
first-write backup is the safety net.

**`next_id` is max-key-plus-one**, and vault rows are keyed by FNV hash, so once a hand-written
event exists the next chat-created event gets a 19-digit id too. Harmless — ids stay unique — but
it makes numeric references in chat unwieldy for events and reminders. Todos keep small ids while
`todos.md` has no hand-written entries. Pre-existing; worth fixing separately.

**Snapshots and tombstones are keyed by `file#id`, not by id.** Primary keys are unique within
their own table, so an event and a todo are both routinely id 1 — on the deployed database they
were. Keying the snapshot by id alone let one row shadow another's: the todo found the event's
snapshot, concluded its line had been deleted, and was never written to the vault. Had `todos.md`
existed, it would have been deleted from the database instead. The "file missing deletes nothing"
guard is what stopped that, which is a reason to keep such guards even when the logic above them
looks sound. Caught on the first production deploy; regression test in `vault::sync`.

**Still one-way:** the worklog and `notes/` — Mervyn reads both and writes neither.

**Git carries the vault to other devices.** The vault directory is the working tree of a private
repo; each cycle is pull → reconcile → commit → push, under the vault lock for the whole of it, so
a file cannot move between a merge measuring its offsets and the write that uses them. Obsidian Git
does the same on a laptop and a phone. Obsidian Sync cannot: it runs inside the Obsidian app and
has no headless client.

**Conflicts pause rather than resolve.** Git merges edits a few lines apart, but in a short file
two *adjacent* lines will not merge — an edit on the phone and a checkbox Mervyn ticked next to it
conflict, and that is the realistic case, not the exotic one. The rebase is aborted, write-back
stops, and Mervyn says so in chat, because a vault that has quietly stopped reaching your phone is
otherwise invisible. Picking a side automatically would mean silently discarding either the phone's
edit or the scheduler's, and Mervyn's commits are not quite reproducible enough to throw away — a
removal consumes a tombstone.

## Decisions

- **Marker style**: HTML comments in the existing single files. Not one-file-per-event; not block ids.
- **Worklog**: out of scope, for the `git pull --ff-only` reason above.
- **Write-back ships disabled** (`[vault] write_back_enabled = false`) so each phase can be
  deployed dark and verified from the logs before it is allowed to touch the vault.
