# Mervyn — Project Specification

## Context

This project is a personal AI assistant (internally called "Mervyn") built for a single user: a professional karaoke jockey and software developer based in Portsmouth, UK. The assistant should feel like a persistent, proactive second brain — not a chatbot. It runs headlessly on a VPS and pushes information to the user rather than waiting to be asked.

The user interacts primarily via Telegram on mobile and desktop (one personal account, no workspace or company tenancy involved). Obsidian is used as the human-readable write surface for adding notes, worklog entries, and reminders. The assistant calls the Anthropic Claude API for all reasoning.

The user is an experienced Rust developer comfortable with async Rust, Tokio, Docker, and GCP/Kubernetes. Code quality expectations are high. Idiomatic Rust is preferred over pragmatic shortcuts.

---

## Goals

- Morning briefing delivered to Telegram each day: agenda, reminders, suggested todo list
- Accept natural-language input via Telegram and route it appropriately (add reminder, log work, ask a question, etc.)
- Sync with Obsidian vault (Markdown files) as the human-facing data layer
- Store structured data (events, reminders, worklog) in `redb` (pure Rust embedded key-value database)
- Call Claude API with assembled context to generate responses and briefings
- Run as a Docker container on a VPS, managed via docker-compose

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                    VPS (Docker)                      │
│                                                      │
│   ┌──────────────────────────────────────────────┐  │
│   │      Telegram long poll (getUpdates, out)    │  │
│   │  update → authorize → ingest → dedupe → route│  │
│   └────────┬──────────────┬──────────────────────┘  │
│            │              │                          │
│   ┌────────▼───┐   ┌──────▼──────┐                 │
│   │ Context    │   │ Claude API  │                  │
│   │ Assembler  │   │ client      │                  │
│   └────────────┘   └─────────────┘                  │
│            │                                         │
│   ┌────────▼───────────────────────┐                │
│   │         Storage layer          │                │
│   │   redb (structured + ingest)   │                │
│   │   Markdown files (Obsidian)    │                │
│   └────────────────────────────────┘                │
│            ▲                                         │
│   ┌────────┴───────────────────────┐                │
│   │  Vault watcher (notify,       │                │
│   │   debounced → sync to redb)   │                │
│   └────────────────────────────────┘                │
│                                                      │
│   ┌──────────────────────────────────────────────┐  │
│   │    Cron scheduler (tokio-cron-scheduler)     │  │
│   │    Briefing, reminders, vault sync (cron)    │  │
│   └──────────────────────────────────────────────┘  │
│                                                      │
│   ┌──────────────────────────────────────────────┐  │
│   │   axum HTTP (local only, nothing dials in)   │  │
│   │   /health + optional /admin/message-ingest   │  │
│   └──────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
         │                        │
   ┌─────▼──────┐         ┌──────▼──────┐
   │  Telegram  │         │  Obsidian   │
   │   (user)   │         │  vault sync │
   └────────────┘         └─────────────┘
```

**Connection direction matters:** both arrows out of the VPS are **outbound**. Mervyn dials `api.telegram.org` (long polling for inbound messages, `sendMessage` for replies) and `api.anthropic.com`; nothing on the internet dials in. There is no webhook route, no tunnel and no reserved domain, and the axum server is not publicly reachable — see **Telegram Integration**.

### Deployment target

Production is expected to run on an **Oracle Cloud** VPS using **Ampere (AArch64/ARM64)** shapes. The stack is ARM-friendly (Rust, Debian-slim, `redb`): use **`linux/arm64`** images—either build on the instance or publish multi-arch (`docker buildx`) from a dev machine so `docker compose pull` works on the VPS without QEMU emulation.

---

## Project Structure

```
mervyn/
├── Cargo.toml
├── Cargo.lock
├── .env.example
├── docker-compose.yml
├── Dockerfile
├── README.md
├── config/
│   └── default.toml          # Non-secret config (schedule times, vault path, etc.)
├── data/
│   ├── mervyn.redb           # redb database file (gitignored)
│   └── vault/                # Obsidian vault sync target (gitignored)
│       ├── worklog.md
│       ├── reminders.md
│       ├── events.md
│       └── notes/
└── src/
    ├── main.rs               # Tokio runtime, startup, long-poll task, service wiring
    ├── config.rs             # Config loading (config crate: file + env + MERVYN__ env overrides)
    ├── error.rs              # Unified error type (thiserror)
    ├── state.rs              # AppState, Secrets (env; hand-written redacting Debug)
    │
    ├── api/
    │   ├── mod.rs            # axum router: /health + optional admin route (no inbound webhook)
    │   └── admin.rs          # Bearer-auth read-only message_ingest listing (if MERVYN_ADMIN_TOKEN set)
    │
    ├── telegram/
    │   ├── mod.rs
    │   ├── client.rs         # Bot API via reqwest: getUpdates / getMe / sendMessage, 4096-char chunking, 429 backoff
    │   ├── updates.rs        # Update + Message serde types and accessors (chat_id, text, is_from_bot)
    │   ├── poller.rs         # Long-poll loop, persisted offset cursor, shutdown watch
    │   └── handler.rs        # Authorize → ingest → dedupe → filter → intent pipeline
    │
    ├── claude/
    │   ├── mod.rs
    │   ├── client.rs         # Anthropic Messages API via reqwest + serde (rustls; see Crate preferences)
    │   ├── payloads.rs       # Typed serde structs for prompt bodies (JSON to Claude)
    │   └── prompts.rs        # Static instructions + JSON builders (no raw format! of user text)
    │
    ├── context/
    │   ├── mod.rs
    │   └── assembler.rs      # Reads redb + vault Markdown, builds context string
    │
    ├── storage/
    │   ├── mod.rs
    │   ├── error.rs          # StorageError (postcard + redb)
    │   ├── codec.rs          # Postcard encode/decode helpers
    │   ├── db.rs             # redb table definitions, open/init
    │   ├── events.rs         # CRUD for Event records
    │   ├── reminders.rs      # CRUD for Reminder records (+ recurrence advance helpers)
    │   ├── worklog.rs        # CRUD for WorklogEntry records
    │   ├── meta.rs           # META_TABLE: update_id dedupe claims + persisted poll offset
    │   └── message_ingest.rs # Ingest log, prune, sweep stale Pending + meta release
    │
    ├── vault/
    │   ├── mod.rs
    │   ├── md.rs             # pulldown-cmark + YAML front matter → domain rows
    │   ├── sync.rs           # Read vault files, call md::parse_*, upsert redb
    │   └── watcher.rs        # notify-debouncer-mini → sync_vault_to_db
    │
    ├── scheduler/
    │   ├── mod.rs
    │   └── jobs.rs           # tokio-cron-scheduler job definitions
    │
    └── intent/
        ├── mod.rs
        ├── add_reminder.rs
        ├── add_event.rs
        ├── log_work.rs
        ├── add_note.rs
        └── ask.rs            # Freeform question → Claude → Telegram reply
```

---

## Dependencies (`Cargo.toml`)

The committed manifest is the source of truth; it is reproduced here for the spec reader.

```toml
[package]
name = "mervyn"
version = "0.1.0"
edition = "2021"
default-run = "mervyn"

[dependencies]
tokio = { version = "1", features = ["full"] }

axum = "0.7"
tower = "0.4"
tower-http = { version = "0.5", features = ["trace"] }

reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }

serde = { version = "1", features = ["derive"] }
serde_json = "1"
postcard = { version = "1", features = ["alloc", "use-std"] }

redb = "2"

tokio-cron-scheduler = "0.10"

notify = "6"
notify-debouncer-mini = { version = "0.4", default-features = false }

config = "0.14"
dotenvy = "0.15"

thiserror = "1"
anyhow = "1"

tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

chrono = { version = "0.4", features = ["serde"] }
chrono-tz = "0.10"

base64 = "0.22"
constant_time_eq = "0.4"
fnv = "1"
pulldown-cmark = { version = "0.13", default-features = false }
serde_yaml = "0.9"

[dev-dependencies]
tempfile = "3"
```

**TLS / `reqwest`:** Mervyn uses **`reqwest`** with **`default-features = false`** and **`rustls-tls`** only, so the binary does not pull **`native-tls`**. Re-evaluate an official or community SDK only if it exposes a rustls-only feature set that preserves that property.

**No crypto dependencies:** `hmac`, `sha2`, `hex` and `bytes` were dropped when the Slack webhook went away — long polling has no inbound request whose signature must be verified, and no raw body to hold. **`constant_time_eq`** remains for the admin Bearer-token comparison only.

**Vault YAML:** **`serde_yaml`** parses optional leading front matter in `vault/md.rs`. The upstream crate is marked deprecated on crates.io; if it stalls, migrate to a maintained YAML library and keep the same `strip_yaml_front_matter` contract.

---

## Crate preferences (avoid reinventing wheels)

Prefer maintained ecosystem crates over hand-rolled logic that duplicates specs and drifts when upstream APIs change.

### Anthropic / Claude HTTP client

**Implemented:** `src/claude/client.rs` POSTs to **`/v1/messages`** with **`reqwest`** (workspace **`rustls-tls`**), header **`anthropic-version: 2023-06-01`**, and serde request/response structs (string user `content`, optional `system`). Plain-text replies concatenate response blocks where **`type` is `text`**.

For streaming, tools, or large API surface area, consider an SDK **only if** it can be configured for rustls-only `reqwest` (see **Dependencies**); otherwise extend the in-tree types carefully.

### Telegram Bot API

**Implemented:** `src/telegram/client.rs` calls the Bot API directly with **`reqwest`** (workspace **`rustls-tls`**): `getUpdates` for long polling, `getMe` on startup, `sendMessage` for replies. Responses deserialize into a small generic `ApiResponse<T>` (`ok`, `description`, `error_code`, `parameters`) instead of ad hoc `serde_json::Value` indexing. The bot token sits in the **URL path** of every call, so `reqwest` errors are mapped through `without_url()` — otherwise a failed request would print the token into the logs.

**There is no signature verification to implement.** Long polling has no inbound request to verify (see **Telegram Integration**); the access control is the single-chat allowlist, and the only remaining constant-time comparison is on the admin Bearer token.

Two Telegram-specific behaviours live in the client, both kept as **pure functions so the policy is unit-testable without I/O**: `split_message` (Telegram caps `sendMessage` text at **4096 UTF-16 code units** — break on a newline where one exists, else on a character boundary) and `classify` (HTTP **429** → sleep `parameters.retry_after`, capped at 60s, max 3 retries per chunk). There is deliberately **no `Notifier` trait**: an async trait is not dyn-compatible without boxing or an added `async-trait` dependency, and there is only one implementation.

If the surface ever grows beyond these endpoints (inline keyboards, media, multi-chat routing), evaluate a maintained bot framework such as **`teloxide`** — but only one that can be configured for rustls-only `reqwest` (see **Dependencies**); otherwise extend the in-tree types carefully.

### Reminder recurrence

`Recurrence::Custom(String)` must not evolve into a home-grown recurrence language. If storing or parsing **iCal-style RRULE** (or equivalent), use an established crate such as **`rrule`** instead of custom string parsers.

### Obsidian / Markdown vault

**Implemented:** `src/vault/md.rs` uses **`pulldown-cmark`** (GFM task lists + ATX headings) for `reminders.md`, `events.md`, and `worklog.md`. Optional Obsidian-style **YAML front matter** (`---` … `---` or `...`) is split and parsed with **`serde_yaml`** before Markdown; the parsed value is available for future file-level metadata and invalid YAML still strips the fence so the body parses. Stay tolerant of messy human edits; worklog **`Tags:`** merged into the last list item (CommonMark tight lists) is handled explicitly.

### Configuration

Layer settings with the **`config`** crate: committed `config/default.toml` plus **environment variables** (and optional local overrides) on the same builder. Do not invent separate precedence or merge rules in application code.

### Storage CRUD

The `events`, `reminders`, and `worklog` modules share the same put/get/delete/list pattern over `redb`. **Implemented:** `src/storage/table.rs` centralises postcard encode/decode + write/read/delete/`next_id` for `TableDefinition<u64, &[u8]>` (also used for `message_ingest::put` / `append` id allocation). Domain modules keep filtered table scans.

### Already aligned (keep as-is)

**`postcard`** for redb values, **`redb`** for embedded storage, **`tokio-cron-scheduler`** for scheduled jobs, **`notify`** for vault file watching, **`serde_json`** for Claude-bound JSON (prompt-injection safety), and **`config`** + **`dotenvy`** for settings are intentional choices and match this philosophy.

---

## Core Types

```rust
// src/storage/events.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,                        // primary key in redb (vault sync uses stable FNV-1a ids; see vault/sync)
    pub title: String,
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub tags: Vec<String>,              // e.g. ["karaoke", "personal", "work"]
}
```

```rust
// src/storage/reminders.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: u64,
    pub body: String,
    pub due: DateTime<Utc>,
    pub recurrence: Option<Recurrence>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Recurrence {
    Daily,
    Weekly,
    Monthly,
    Custom(String),  // e.g. RRULE or cron — parse with a real library (see Crate preferences)
}
```

```rust
// src/storage/worklog.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorklogEntry {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub body: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
}
```

```rust
// src/storage/db.rs

use redb::{Database, TableDefinition};

// Tables: value = postcard-serialised struct bytes.
// u64 tables: domain ids (vault-derived rows use stable FNV-1a keys; others set id explicitly);
// message_ingest uses monotonic u64 row keys. META uses &str keys.
pub const EVENTS_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("events");

pub const REMINDERS_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("reminders");

pub const WORKLOG_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("worklog");

// Key-value metadata: update dedupe claims (`tg:update:{update_id}`)
// and the persisted long-poll cursor (`tg:poll_offset`).
pub const META_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("meta");

// Append-only delivery log keyed by monotonic u64 (see storage/message_ingest)
pub const MESSAGE_INGEST_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("message_ingest");

pub fn open(path: &str) -> anyhow::Result<Database> {
    let db = Database::create(path)?;
    let write_txn = db.begin_write()?;
    {
        let _ = write_txn.open_table(EVENTS_TABLE)?;
        let _ = write_txn.open_table(REMINDERS_TABLE)?;
        let _ = write_txn.open_table(WORKLOG_TABLE)?;
        let _ = write_txn.open_table(META_TABLE)?;
        let _ = write_txn.open_table(MESSAGE_INGEST_TABLE)?;
    }
    write_txn.commit()?;
    Ok(db)
}
```

The committed `src/storage/db.rs` also defines `TODOS_TABLE` and `EVENT_NOTICES_TABLE`; the block above is the transport-relevant core. The `slack_ingest` table was renamed to `message_ingest` with the Telegram cutover — the database was wiped in the same session, so no redb migration was needed.

### Claude client (`src/claude/client.rs`) — as implemented

`ClaudeClient` holds a `reqwest::Client`, API key, `model`, and `max_tokens`. `complete(system, user_message)` sends one user message with string content and optional `system`, then joins `text` content blocks from the JSON response. HTTP and JSON failures surface as `anyhow::Error`.

See **Dependencies** above for the rustls-only `reqwest` setup.

**Context assembler** — Implemented in `src/context/assembler.rs`: `build_briefing_context`, `build_query_context`, and `briefing_prompt_sections` (three blobs for `MorningBriefingV1`). All take an explicit `now: DateTime<Utc>`.

---

## Telegram Integration

Mervyn uses the Telegram **Bot API** over **long polling** (`getUpdates`), **not** webhooks. The process dials out to `api.telegram.org` and holds each request open for up to 30 seconds; nothing dials in. That removes the whole inbound surface a webhook would need — no public route, no HMAC signature verification on a raw body, no tunnel, no reserved domain, no TLS certificate for the app — at the cost of the transport no longer authenticating anything (see **Authorization** below).

### Setup steps (manual, one-time)

1. Create a bot by messaging **@BotFather** on Telegram (`/newbot`): display name, then a username ending in `bot`. No workspace or company tenancy is involved — the user's own Telegram account on Desktop / web / iOS / Android is the client.
2. Copy the HTTP API token into `.env` as **`TELEGRAM_BOT_TOKEN`**.
3. Message the bot once, then read `result[].message.chat.id` from `https://api.telegram.org/bot<TOKEN>/getUpdates` and put that number in `.env` as **`TELEGRAM_CHAT_ID`** (positive for a direct chat, negative for a group). Do this before starting Mervyn — two pollers on one token compete for updates.
4. For a group chat only: turn **Group Privacy** off in BotFather so ordinary, non-mentioning messages reach the bot.

### Authorization: the single-chat allowlist

**Long polling has no transport-level authentication.** A webhook could at least be checked against a signing secret; here, any Telegram user who discovers the bot's username can message it and Telegram will deliver those updates. **`TELEGRAM_CHAT_ID` is therefore the only access control in the system** and must be treated as a security control, not a convenience setting.

- `telegram::handler::is_authorized` compares `update.chat_id()` with the configured id and runs **before the ingest write and before any Claude call**. An unauthorized sender is logged at `warn` and dropped: it cannot grow the database, cannot spend Anthropic quota, and gets no reply. (The originally planned `FilteredUnauthorizedSender` ingest outcome was deliberately dropped — recording a stranger's traffic would have handed them a write primitive.)
- `Secrets::from_env` parses the id as `i64` and **rejects `0`**, so a misconfigured or empty value cannot pair with a chat-less update and compare equal.
- `Secrets` does **not** derive `Debug`; it implements a redacting one, because a single `{:?}` would otherwise dump every credential. The bot token is doubly sensitive: it travels in the **URL path** of every Bot API call, so `reqwest` errors are mapped through `without_url()`.
- Anyone holding the token can read every update the bot receives and post as it. Rotate via BotFather if it leaks.

### Long-poll loop and cursor

`telegram/poller.rs` runs as a Tokio task alongside the axum server and stops when the server begins draining (a `watch` channel). `getUpdates` is called with `timeout=30` and an `offset`, which is both the **server-side acknowledgement** (Telegram stops resending anything below it) and the replay guard. The offset is persisted in `META_TABLE` under `tg:poll_offset`, so a restart resumes instead of replaying, and it advances **after** a batch is handled — a crash mid-batch redelivers, which is why the per-update dedupe claim below is still required. A failed poll backs off 5 seconds rather than hot-looping, and the cursor advances past the whole batch regardless of individual outcomes so one poisoned update cannot wedge the loop into redelivering it forever.

### Delivery ingest, deduplication, and handler pipeline

Every **authorized** update is recorded before any business logic runs, so redeliveries and filtered noise still leave an audit trail in redb.

| Step | Responsibility | Code |
|------|----------------|------|
| 0. **Authorize** | Drop any update whose `chat.id` is not `TELEGRAM_CHAT_ID` — before any write and before Claude. | `telegram::handler::is_authorized` |
| 1. **Ingest** | Append one row per update to `message_ingest` (`MESSAGE_INGEST_TABLE`): `event_id` (the Telegram `update_id`), `received_at_ms`, `inner_type` (`text` or `other`), initial outcome `Pending`. | `storage/message_ingest::append` |
| 2. **Dedupe** | Atomically claim the `update_id` in `META_TABLE` (`tg:update:{update_id}`). If the claim fails, this is a redelivery of an already-handled update: set outcome `DuplicateDelivery` and stop (no Claude, no reply). | `storage/meta::try_claim_delivery` |
| 3. **Filter** | Drop bot messages, non-message updates, and non-text or empty messages. Release the dedupe claim when skipping so a later legitimate redelivery can run. Set outcome (`FilteredBot`, `FilteredNonText`, `FilteredEmptyText`). | `telegram/handler.rs` |
| 4. **Action** | Classify intent → dispatch. On success: outcome `Processed`. On handler `Err`: **release** the dedupe claim (so a redelivery can retry), outcome `Failed` (truncated error string). | `intent/*` |

The `retry_num` column survives from the Slack-era schema and is always `None` under Telegram — long polling has no per-delivery retry counter, and dedupe is keyed on `update_id` regardless.

**Hardening:** A [`DeliveryClaimGuard`](src/telegram/handler.rs) releases the meta claim on drop (e.g. a panic) after a successful claim, unless disarmed on the success path. Rows still stuck `Pending` (panic before the guard is installed, or a persistence failure after disarm) are cleared by the scheduled [`sweep_stale_pending`](src/storage/message_ingest.rs) run (same cron as prune; see **`message_ingest_stale_pending_minutes`**).

### Outbound replies

Replies go out via `sendMessage` as **plain text with no `parse_mode`**: Claude's output is Markdown-ish and appointment reminders embed curly quotes, every one of which `MarkdownV2` would demand be escaped. Text longer than Telegram's **4096-unit** cap is split by `split_message` (newline break preferred, character boundary otherwise, counting **UTF-16 code units** because that is what the cap measures — an emoji is one `char` but two units). HTTP **429** is honoured via `parameters.retry_after`, capped at 60s and at 3 retries per chunk; Telegram's per-chat limit is roughly one message per second and the reminder job posts serially in a loop.

### Operator: `message_ingest` listing

When **`MERVYN_ADMIN_TOKEN`** is set (non-empty), the router mounts **`GET /admin/message-ingest`**. Clients send **`Authorization: Bearer <token>`** (compared with **`constant_time_eq`** on the suffix; reject wrong length without leaking timing on the secret). Query parameters (all optional except as noted):

| Param | Meaning |
|-------|---------|
| `limit` | Max rows, default **100**, clamped **1..=500** |
| `since_ms` / `until_ms` | Inclusive window on `received_at_ms` (Unix millis) |
| `outcome` | Variant filter: `Pending`, `Processed`, `Failed` (any failure), `DuplicateDelivery`, `FilteredBot`, … |
| `event_id` | Exact match on the stored Telegram `update_id` |

Response JSON: `{ "rows": [ { "id", "event_id", "received_at_ms", "retry_num", "inner_type", "outcome" } ] }` where `outcome` is a short string (`Failed: …` includes the message). Implementation scans the ingest table in memory (`storage::message_ingest::list_recent`) — for debugging only, not a high-QPS API. If **`MERVYN_ADMIN_TOKEN`** is unset, the route is **not registered** (no probe surface). The route is on the same local-only server as `/health` and is not published to the internet.

---

## Prompt design (JSON API)

User-derived text must **not** be interpolated into prompt templates with `format!`. Use typed payloads in `src/claude/payloads.rs` (`Serialize`) and **`serde_json`** for the strings sent to Claude. JSON is used (not TOML): it handles nested text, multiline chat messages, and escaping automatically; TOML remains for human-edited config files only.

### Layout

| Piece | Where | Role |
|--------|--------|------|
| Session context | `SystemContextV1` (+ nested profile / clock) | `assistant_name`, user facts, London date/time, `api_version` |
| Task-specific user bodies | `IntentClassificationV1`, `MorningBriefingV1`, `FreeformQueryV1` | `task` discriminator + string fields (raw chat text lives in JSON string values) |
| Static prose | `prompts::SYSTEM_CORE`, `SUPPLEMENT_*` | Fixed `&'static str`; append the right supplement to `system` per call type |

### Call shape

- **`system`**: `prompts::system_prompt_json(now)?` plus, when needed, `\n\n` + `SUPPLEMENT_INTENT_CLASSIFICATION` / `SUPPLEMENT_MORNING_BRIEFING` / `SUPPLEMENT_FREEFORM_QUERY`.
- **`user` (Claude “user” message content)**: a **single JSON object** from `intent_classification_user_json`, `morning_briefing_user_json`, or `freeform_query_user_json` (each returns `serde_json::Result<String>`).

Constants `PROMPT_API_VERSION` and `TASK_*` in `payloads.rs` identify the schema for future migrations.

### Behaviour notes (intent classification)

- Intent routing still resolves to exactly one of: `add_reminder`, `add_event`, `log_work`, `add_note`, `remove_event`, `complete_todo`, `remember_briefing`, `ask`. The supplement asks the model for a **single JSON object** with `api_version` and `intent` (snake_case); `intent/mod.rs` parses JSON first, then markdown-fenced JSON, then a legacy plain-text label for compatibility.
- Morning briefing still asks for summary, due/overdue reminders, and a prioritised todo list (max 7); the three context blobs are JSON fields on `MorningBriefingV1`.

---

## Scheduler Jobs

Implemented with `tokio-cron-scheduler`. Cron expressions and timezone come from **`config/default.toml`** (`[scheduler]` — `morning_briefing_cron`, `reminder_check_cron`, `vault_sync_cron`, `message_ingest_prune_cron`, `timezone`, e.g. `Europe/London` with DST). Defaults match the table below; override via TOML or `MERVYN__SCHEDULER__*` env vars.

| Job | Default schedule | Description |
|-----|------------------|-------------|
| `morning_briefing` | `0 30 7 * * *` | Assemble context, call Claude, post to `TELEGRAM_CHAT_ID` |
| `reminder_check` | `0 * * * * *` | Due pending reminders → Telegram (serially, so the client's 429 backoff matters) |
| `vault_sync` | `0 */5 * * * *` | Re-read vault Markdown files into redb |
| `message_ingest_prune` | `0 0 4 * * *` | Age + row-cap pruning (`message_ingest::prune`) and stale-`Pending` sweep (`message_ingest::sweep_stale_pending`) |

**Vault watcher:** In addition to the cron job, `src/vault/watcher.rs` watches the vault tree with **`notify`**, debounces, and calls `sync_vault_to_db` so Obsidian saves land in redb quickly.

The reminder check job uses storage helpers that query due pending reminders (not a blind full-table scan where avoidable).

---

## Vault Markdown Format

Obsidian is the human write surface. The sync job reads these files and upserts structured records into redb. The canonical format for each file:

### `vault/reminders.md`

```markdown
# Reminders

- [ ] Call accountant about VAT return — due 2026-04-05
- [ ] Renew car insurance — due 2026-04-12 — recurs yearly
- [x] Book venue for April gig — done
```

### `vault/events.md`

```markdown
# Events

## 2026-04-05 — Karaoke night, The Tap Room
Doors 7pm, finish ~11pm. Bring SM58 and spare batteries.
Tags: karaoke, work

## 2026-04-10 — Dentist
14:30, Southsea Dental Centre
Tags: personal
```

### `vault/worklog.md`

```markdown
# Worklog

## 2026-03-30
- Worked on OpenKJ rewrite: queue scoring algorithm, added recency decay
- Mervyn project: initial architecture sketch
Tags: openkj, mervyn

## 2026-03-29
- Karaoke at The Tap Room, good night, ~80 requests
Tags: karaoke
```

**Current code:** `src/vault/md.rs` drives structure extraction (**`pulldown-cmark`** + optional **`serde_yaml`** front matter); `src/vault/sync.rs` reads files and upserts into redb. Stable row IDs use **FNV-1a 64-bit** via the **`fnv`** crate in `md::stable_vault_row_id` (prefix + normalised key string). You may add an Obsidian YAML block at the top of any vault file without breaking list/heading parsing.

---

## Configuration

Load defaults from `config/default.toml` and merge **environment variables** (and optional overrides) via the **`config`** crate as described in **Crate preferences (avoid reinventing wheels)**.

### `.env` (secrets, never committed)

Required variables are loaded in `Secrets::from_env` (`src/state.rs`). See **`.env.example`** in the repo for the full list and optional `MERVYN__` overrides.

```env
ANTHROPIC_API_KEY=sk-ant-...
TELEGRAM_BOT_TOKEN=123456789:AA...
TELEGRAM_CHAT_ID=123456789  # The ONLY chat Mervyn answers, and where briefings/reminders go (numeric; negative for groups)
# Optional: MERVYN_ADMIN_TOKEN=...   # Enables GET /admin/message-ingest (Bearer)
```

### `config/default.toml` (non-secret, committed)

```toml
[claude]
model = "claude-sonnet-4-6"
max_tokens = 2048

[scheduler]
morning_briefing_cron = "0 30 7 * * *"
reminder_check_cron = "0 * * * * *"
vault_sync_cron = "0 */5 * * * *"
message_ingest_prune_cron = "0 0 4 * * *"
timezone = "Europe/London"

[storage]
db_path = "./data/mervyn.redb"
vault_path = "./data/vault"
message_ingest_retention_days = 90
message_ingest_keep_last = 100000
message_ingest_stale_pending_minutes = 30

[server]
port = 3000
```

---

## Docker

### `Dockerfile`

```dockerfile
FROM rust:1.77-slim-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
# Cache deps layer
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/mervyn .
COPY config ./config
VOLUME ["/app/data"]
EXPOSE 3000
CMD ["./mervyn"]
```

### `docker-compose.yml`

```yaml
services:
  mervyn:
    build: .
    restart: unless-stopped
    ports:
      - "3000:3000"
    volumes:
      - ./data:/app/data
    env_file:
      - .env
    environment:
      - RUST_LOG=mervyn=debug,tower_http=info
```

---

## Implementation Order

Build and validate each layer before moving to the next. Each step should be independently testable. **As of the current tree, steps 1–11 are largely implemented** (storage, vault sync, Claude Messages client, context, prompts, scheduler, Telegram client, long-poll pipeline with ingest/dedupe, intents, Docker assets); use the checklist below for remaining hardening (see **Open TODOs**) and optional refactors from **Crate preferences**.

1. **Storage layer** — `src/storage/`. Define tables, implement CRUD for all three record types (shared helpers in `storage/table.rs` per **Crate preferences**). Write unit tests using a temp file path for the database.

2. **Storage read strategy (spike)** — Before vault sync and context assembly assume unbounded data, evaluate **paging / key-range scans / caps** in redb instead of loading whole tables with `list_all` → `Vec`. Decide patterns for: upcoming events window, pending reminders, recent worklog, reminder sweep, and any “dump for Claude” paths. Outcome should guide `context/assembler` and job implementations.

3. **Vault parser** — `src/vault/md.rs` + `sync.rs`. Parse the three Markdown files (**`pulldown-cmark`**, optional YAML via **`serde_yaml`**) into typed structs; unit tests in `vault/md.rs` / `vault/sync.rs`. Upsert into redb.

4. **Claude client** — `src/claude/client.rs`. Implement the HTTP client and `complete()` method per **Crate preferences** (prefer a maintained client or generated types for the Messages API). Test with a hardcoded prompt against the real API.

5. **Context assembler** — `src/context/assembler.rs`. Implement `build_briefing_context()`. Test by printing the assembled string (use the read strategy from step 2).

6. **Prompt API** — `src/claude/payloads.rs` + `prompts.rs`. Wire callers to `system_prompt_json`, `*_user_json`, and `SUPPLEMENT_*` as in **Prompt design (JSON API)** above. Do not add new `format!`-based user interpolation.

7. **Scheduler** — `src/scheduler/jobs.rs`. Wire up cron jobs. Test morning briefing job end-to-end (storage → context → Claude → print output).

8. **Telegram client** — `src/telegram/client.rs`. Implement `send_message()` (with chunking and 429 backoff) and `get_updates()`. Test by posting to the allowlisted chat.

9. **Telegram long-poll receiver** — `src/telegram/poller.rs` + `handler.rs`. Long-poll loop with a persisted offset, then **authorize → ingest → dedupe → filter → action** (see *Delivery ingest, deduplication, and handler pipeline*), intent routing. No inbound HTTP route is involved.

10. **Intent handlers** — `src/intent/`. Implement each handler. Wire everything together in `main.rs`.

11. **Docker** — Build image, test locally, deploy to VPS. **No reverse proxy or TLS termination is required**: the container needs outbound HTTPS only, and port 3000 serves nothing that should be public.

---

## Open TODOs (engineering follow-ups)

The checklist below tracks production hardening; **core items are implemented** — extend with metrics/alerts, TLS fronting, and richer parsers per **Crate preferences** as needed.

- [x] **`message_ingest` retention** — Implemented: `storage::message_ingest::prune` (age in days, then optional max row count by monotonic id), scheduled job `message_ingest_prune` in `scheduler/jobs.rs`. Knobs: `storage.message_ingest_retention_days`, `storage.message_ingest_keep_last` (use `0` to disable each rule), `scheduler.message_ingest_prune_cron`; env `MERVYN__STORAGE__MESSAGE_INGEST_*`, `MERVYN__SCHEDULER__MESSAGE_INGEST_PRUNE_CRON`.
- [x] **Stuck `Pending` ingest rows** — Implemented: `DeliveryClaimGuard` in `telegram/handler.rs` releases the dedupe meta on drop after a claim (disarmed on success/filter/error paths); `message_ingest::sweep_stale_pending` on the prune cron marks long-`Pending` rows failed and releases meta (`storage.message_ingest_stale_pending_minutes`, `0` = off). Metrics/alerts left to deployment.
- [x] **Operator visibility** — Implemented: `GET /admin/message-ingest` when `MERVYN_ADMIN_TOKEN` is set; Bearer auth; query filters `limit`, `since_ms`, `until_ms`, `outcome`, `event_id`; JSON body via `api/admin.rs` + `message_ingest::list_recent`.
- [ ] **Unauthorized-sender visibility** — Updates from other chats are dropped with a `warn` log and nothing persisted (deliberate: see **Authorization**). If the bot's username ever leaks widely, consider a counter or rate-limited log so a sustained probe is noticeable without giving strangers a write path.

---

## Notes for the Implementer

- Read **Crate preferences (avoid reinventing wheels)** before touching the Telegram client, Claude HTTP, vault parsing, recurrence, config layering, and storage CRUD patterns.
- Use `Arc<redb::Database>` everywhere — the database handle is shared across the scheduler, the HTTP handler, cron-driven vault sync, and the **vault watcher thread** (notify debounce → `sync_vault_to_db`).
- Immediate vault updates are driven by **`notify`** + **`notify-debouncer-mini`** in `vault/watcher.rs`, not by a Tokio broadcast channel (a broadcast channel remains an option if you add non-filesystem writers later).
- Keep Claude-facing text out of the transport and intent handlers: build `system` / user JSON via `src/claude/prompts.rs` and `payloads.rs` only. Handlers pass structured inputs into those APIs; do not add ad hoc `format!` with user-controlled text.
- The vault sync is one-directional for now: Obsidian → redb. Mervyn does not write back to the vault Markdown files in this initial version.
- Error handling: use `anyhow` for application-level errors, `thiserror` for library-level error types. Never `.unwrap()` in async task bodies — a panic in a spawned task is silent unless you explicitly handle the `JoinHandle`.
- Tracing: instrument every significant operation with `tracing::info!` / `tracing::debug!` spans. The Docker logs are your only observability.
- Telegram: the single-chat allowlist is the only access control — never move the authorization check after a write or a Claude call (see *Authorization: the single-chat allowlist*). Ingest retention, the stale-`Pending` sweep, and the optional admin listing (`MERVYN_ADMIN_TOKEN`) are covered in *Operator: message_ingest listing*.
