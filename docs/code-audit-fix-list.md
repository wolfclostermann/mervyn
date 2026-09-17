# Mervyn code audit — fix list

Structured backlog from a full pass over the Rust codebase (storage, schedulers, Slack pipeline, intents, context). Deployment scripts and Terraform were not audited in depth.

---

## Critical / correctness risks

1. **Reminder firing loop is not transactional** (`src/scheduler/jobs.rs`) — **addressed
   2026-09-17, but read the caveat.**

   It cannot be made transactional. `sendMessage` has no idempotency key, so send-and-persist
   cannot be one atomic step, and a failure between them loses something whichever order they run
   in: persist-first risks a reminder that never arrives, send-first risks announcing it twice.
   The suggestion to persist before sending trades a duplicate for a miss, and a miss defeats the
   purpose of a reminder — so the order stands, deliberately, and is now documented at the
   function.

   What *was* wrong and is fixed:
   - **One failure aborted the whole tick.** `send_message(...).await?` meant a single Telegram
     hiccup skipped every later reminder *and* the appointment sweep, which had nothing to do with
     it. Each reminder is now isolated, and appointments run regardless.
   - **Duplicates were unbounded.** A row that kept failing to write re-announced every minute for
     ever. `storage/reminder_notices` counts announcements per occurrence and stops after three,
     logging loudly; the stuck row is visible in `/admin/reminders`.
   - The same abort-the-batch flaw in `appointment_reminders` is fixed the same way, per event and
     per message, so a failed advance notice no longer marks itself sent.

   Still true: a crash in the window between a successful send and the row write re-announces once
   on the next tick. That is the chosen trade, not an oversight. A wholly unwritable database is
   beyond the bound, since the counter cannot be written either.

2. **Appointment reminders scan the entire events table every tick** (`src/scheduler/appointment_reminders.rs` uses `events::list_all`)  
   As events accumulate this becomes O(n) per cron tick with full decode. Prefer a bounded query such as `upcoming_within(now, now + horizon, max)` so past events are not scanned forever.

3. **`complete_todo` silently treats bad model JSON as “complete nothing”** (`src/intent/complete_todo.rs`)  
   When `parse_complete_reply` fails, the code logs a warning and uses an empty `Vec`, which surfaces as the generic “No todos were marked done” reply. Users cannot distinguish parse failure from the model returning no ids. Consider a distinct user-visible message when JSON parsing fails vs an empty id list.

4. ~~**Vault sync overwrites DB rows by shared ids**~~ — **fixed 2026-09-17.** Items carry their id
   in the Markdown as an HTML comment, so identity no longer depends on hashing content and an
   edited line keeps its row. Hashing survives only to bootstrap a line that has no marker yet.
   See `docs/two-way-vault-sync.md`.

---

## Consistency / maintainability

5. **`scheduler.timezone` validation differs by call site**  
   - `spawn_scheduler`: invalid IANA zone fails startup.  
   - `add_event` / `add_reminder`: invalid zone falls back to UTC with a warning.  
   Unify behavior: fail fast at config load for all paths, or one shared helper (e.g. `AppConfig::scheduler_tz()`) used everywhere.

6. **Repeated `map_err(|e| anyhow::anyhow!(e))` on storage errors**  
   Widespread across intents, jobs, and `context/assembler.rs`. Consider `From`/`Context` patterns or a thin helper so errors chain consistently.

7. **`pending_due_by` duplicates `pending_due_within`** (`src/storage/reminders.rs`)  
   Same scan/filter logic; `pending_due_by(db, at, max)` matches `pending_due_within(db, at, max)`. Implement one in terms of the other to avoid drift.

8. **Duplicate “parse model JSON + markdown fence” flows**  
   Similar logic appears in `intent/add_event_claude_time.rs`, `intent/complete_todo.rs`, and related paths (alongside `normalize_claude_json_block`). Extract one helper for parse-with-fallback.

9. **`text_datetime` allocates aggressively** (`src/intent/text_datetime.rs`)  
   Helpers like `has_whole_word` and several parsers call `text.to_lowercase()` repeatedly on user input. For long Slack messages, consider byte/ASCII folding or a single lowercased buffer per top-level entry point.

---

## Operational / product behavior

10. **Intent classification failure defaults to `Ask`** (`src/slack/handler.rs`)  
    Safe default but may send confusing traffic down the wrong path. Optional: single retry, clearer logging/metrics, or a short “couldn’t classify” reply instead of full Q&A.

11. **Slack route returns 200 before async work completes** (`src/api/mod.rs`)  
    Correct for Slack timeouts; failures are log-only unless Slack retries. Operational clarity: rely on `slack_ingest` outcomes + dashboards/runbooks.

12. **Cron expressions not validated at config load** (`src/config.rs`, `scheduler/jobs.rs`)  
    Bad crons surface when `Job::new_async_tz` runs. Validate or parse at startup with explicit errors.

13. **`due_datetime_from_reminder_text` uses `.expect("valid time")`** (`src/intent/text_datetime.rs`)  
    Low risk for fixed 09:00 on “tomorrow,” but inconsistent with surrounding style. Prefer a non-panicking fallback.

---

## Data model / evolution

14. **Postcard blobs have no schema version** (`src/storage/codec.rs`)  
    Serde shape changes can brick decoding of existing rows. Plan migrations, optional version fields on structs, or a documented “reset DB on upgrade” policy.

15. **`slack_ingest::set_outcome` succeeds silently if the row id is missing** (`src/storage/slack_ingest.rs`)  
    Can hide bugs. Consider a `tracing::warn` or stricter behavior in debug/tests.

---

## Performance (likely fine at small scale)

16. **Full table scans for filtered domain queries** (`events::upcoming_within`, `reminders::pending_*`, etc.)  
    Acceptable when documented; if data grows, consider tighter iteration or secondary structures.

17. **`ClaudeClient` holds one `reqwest::Client`** (`src/claude/client.rs`)  
    Usually fine; revisit pool/timeouts if concurrency increases.

---

## Tests / hygiene

18. **`user_situation` tests embed cron strings** that may drift from production defaults — acceptable if intentional; otherwise sync or minimize duplication.

19. **`build_briefing_context` is `dead_code`** (`src/context/assembler.rs`)  
    Wire to a debug/operator path or remove to reduce noise.

---

## Suggested prioritization

| Priority | Items |
|----------|--------|
| **P0** | ~~1 (reminder consistency)~~ addressed, 2 (appointment full scan) |
| **P1** | 5 (timezone split — partly addressed: vault times are now local wall-clock via `local_time.rs`), 3 (complete_todo parse vs empty) |
| **P2** | 6, 7, 8 (DRY + errors), 12 (cron validation), 14 (serialization/version story) |
| **P3** | 9, 16 (perf polish), 10, 11 (observability/UX), 13, 15, 17–19 |

---

*Generated from an internal audit; update this file as items are fixed or reprioritized.*
