# Mervyn — Project Specification

## Context

This project is a personal AI assistant (internally called "Mervyn") built for a single user: a professional karaoke jockey and software developer based in Portsmouth, UK. The assistant should feel like a persistent, proactive second brain — not a chatbot. It runs headlessly on a VPS and pushes information to the user rather than waiting to be asked.

The user interacts primarily via Slack on mobile and desktop. Obsidian is used as the human-readable write surface for adding notes, worklog entries, and reminders. The assistant calls the Anthropic Claude API for all reasoning.

The user is an experienced Rust developer comfortable with async Rust, Tokio, Docker, and GCP/Kubernetes. Code quality expectations are high. Idiomatic Rust is preferred over pragmatic shortcuts.

---

## Goals

- Morning briefing delivered to Slack each day: agenda, reminders, suggested todo list
- Accept natural-language input via Slack and route it appropriately (add reminder, log work, ask a question, etc.)
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
│   │              Orchestrator (axum)             │  │
│   │         Slack event receiver + router        │  │
│   └────────┬──────────────┬─────────────────────┘  │
│            │              │                          │
│   ┌────────▼───┐   ┌──────▼──────┐                 │
│   │ Context    │   │ Claude API  │                  │
│   │ Assembler  │   │ client      │                  │
│   └────────────┘   └─────────────┘                  │
│            │                                         │
│   ┌────────▼───────────────────────┐                │
│   │         Storage layer          │                │
│   │   redb (structured)            │                │
│   │   Markdown files (Obsidian)    │                │
│   └────────────────────────────────┘                │
│                                                      │
│   ┌──────────────────────────────────────────────┐  │
│   │    Cron scheduler (tokio-cron-scheduler)     │  │
│   │    Morning briefing, reminder checks         │  │
│   └──────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
         │                        │
   ┌─────▼──────┐         ┌──────▼──────┐
   │   Slack    │         │  Obsidian   │
   │  (user)    │         │  vault sync │
   └────────────┘         └─────────────┘
```

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
    ├── main.rs               # Tokio runtime, startup, service wiring
    ├── config.rs             # Config loading (config crate: file + env + MERVYN__ env overrides)
    ├── error.rs              # Unified error type (thiserror)
    ├── state.rs              # AppState, Secrets (env)
    │
    ├── api/
    │   └── mod.rs            # axum router, Slack event handler HTTP endpoint
    │
    ├── slack/
    │   ├── mod.rs
    │   ├── client.rs         # Outbound Slack Web API calls (reqwest)
    │   ├── events.rs         # Envelope serde, signature verify, X-Slack-Retry-Num parse
    │   └── handler.rs        # Ingest → dedupe → filter → intent pipeline
    │
    ├── claude/
    │   ├── mod.rs
    │   ├── client.rs         # Anthropic Messages API client (prefer maintained SDK/types; see Crate preferences)
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
    │   ├── meta.rs           # META_TABLE: Slack event_id dedupe claims
    │   └── slack_ingest.rs   # Append ingest log + outcome updates per delivery
    │
    ├── vault/
    │   ├── mod.rs
    │   ├── sync.rs           # Vault Markdown → redb upsert
    │   └── watcher.rs        # notify debounced re-sync on vault file changes
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
        └── ask.rs            # Freeform question → Claude → Slack reply
```

---

## Dependencies (`Cargo.toml`)

```toml
[package]
name = "mervyn"
version = "0.1.0"
edition = "2021"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Web framework (Slack event receiver)
axum = "0.7"
tower = "0.4"
tower-http = { version = "0.5", features = ["trace"] }

# HTTP client (Slack API + Claude API)
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
postcard = { version = "1", features = ["alloc"] }  # redb value serialization

# Database
redb = "2"

# Scheduling
tokio-cron-scheduler = "0.10"

# File watching (vault)
notify = "6"

# Config
config = "0.14"
dotenvy = "0.15"

# Error handling
thiserror = "1"
anyhow = "1"

# Logging/tracing
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Time
chrono = { version = "0.4", features = ["serde"] }
```

---

## Crate preferences (avoid reinventing wheels)

Prefer maintained ecosystem crates over hand-rolled logic that duplicates specs and drifts when upstream APIs change.

### Anthropic / Claude HTTP client

When wiring `src/claude/client.rs`, prefer an **official or well-maintained community client** (or OpenAPI-generated types) for request/response shapes, headers, API versioning, streaming, and tool use. Hand-written `serde` structs are fine for early spikes but must not be the long-term single source of truth for the Messages API.

### Slack Events API

**Signature verification** must follow Slack’s rules: timestamp freshness, payload `v0:{timestamp}:{raw_body}`, HMAC-SHA256 with the signing secret, and **constant-time** comparison on the digest — using the **raw request body** before JSON parsing. Implement with the **`hmac`** and **`sha2`** crates and Slack’s docs, or use a **small, focused Slack signing helper** that encodes the same algorithm so behaviour stays aligned with [Verifying requests from Slack](https://api.slack.com/authentication/verifying-requests-from-slack). For heavier typing of envelopes and events, evaluate Slack-oriented crates; otherwise keep **`serde`** for minimal shapes.

### Reminder recurrence

`Recurrence::Custom(String)` must not evolve into a home-grown recurrence language. If storing or parsing **iCal-style RRULE** (or equivalent), use an established crate such as **`rrule`** instead of custom string parsers.

### Obsidian / Markdown vault

Vault sync and Markdown parsing should use a real **Markdown parser** (e.g. **`pulldown-cmark`** or **`comrak`**) and, for YAML front matter, a **`gray_matter`-style** crate. Stay tolerant of messy human edits in the ways this spec already allows, but implement structure extraction with proper parsers rather than fragile ad-hoc scanners where those crates apply.

### Configuration

Layer settings with the **`config`** crate: committed `config/default.toml` plus **environment variables** (and optional local overrides) on the same builder. Do not invent separate precedence or merge rules in application code.

### Storage CRUD

The `events`, `reminders`, and `worklog` modules share the same put/get/delete/list pattern over `redb`. Use a **macro or generic helper** over `TableDefinition` to avoid duplicated transaction/commit patterns and inconsistent error handling.

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
    pub id: u64,                        // unix timestamp of creation (key in redb)
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

// Tables: key = u64 (unix epoch millis), value = postcard-serialised struct bytes
pub const EVENTS_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("events");

pub const REMINDERS_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("reminders");

pub const WORKLOG_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("worklog");

// Key-value metadata table (last sync time, user prefs, etc.)
pub const META_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("meta");

pub fn open(path: &str) -> anyhow::Result<Database> {
    let db = Database::create(path)?;
    // Initialise tables if they don't exist
    let write_txn = db.begin_write()?;
    write_txn.open_table(EVENTS_TABLE)?;
    write_txn.open_table(REMINDERS_TABLE)?;
    write_txn.open_table(WORKLOG_TABLE)?;
    write_txn.open_table(META_TABLE)?;
    write_txn.commit()?;
    Ok(db)
}
```

Illustrative shape only — prefer a maintained Anthropic client or generated types for production (see **Crate preferences**).

```rust
// src/claude/client.rs

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ClaudeRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    pub messages: Vec<ClaudeMessage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaudeMessage {
    pub role: String,   // "user" or "assistant"
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ClaudeResponse {
    pub content: Vec<ClaudeContent>,
}

#[derive(Debug, Deserialize)]
pub struct ClaudeContent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
}

pub struct ClaudeClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            model,
        }
    }

    pub async fn complete(
        &self,
        system: Option<&str>,
        user_message: &str,
    ) -> anyhow::Result<String> {
        let req = ClaudeRequest {
            model: self.model.clone(),
            max_tokens: 2048,
            system: system.map(String::from),
            messages: vec![ClaudeMessage {
                role: "user".into(),
                content: user_message.into(),
            }],
        };

        let resp = self.http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json::<ClaudeResponse>()
            .await?;

        let text = resp.content
            .into_iter()
            .filter_map(|c| c.text)
            .collect::<Vec<_>>()
            .join("");

        Ok(text)
    }
}
```

**Context assembler** — Implemented in `src/context/assembler.rs`: `build_briefing_context`, `build_query_context`, and `briefing_prompt_sections` (three blobs for `MorningBriefingV1`). All take an explicit `now: DateTime<Utc>`.

---

## Slack Integration

Mervyn uses the Slack **Events API** (HTTP POST) rather than a persistent WebSocket connection. Slack sends a POST to your VPS endpoint when a message is received.

### Setup steps (manual, one-time)

1. Create a Slack app at api.slack.com/apps
2. Enable **Event Subscriptions**, set the Request URL to `https://your-vps-domain.com/slack/events`
3. Subscribe to the `message.channels` and `app_mention` bot events
4. Add OAuth scopes: `chat:write`, `channels:history`, `app_mentions:read`
5. Install the app to your workspace
6. Copy the **Bot Token** (`xoxb-...`) and **Signing Secret** into `.env`

### Signature verification

Every incoming Slack event must be verified using the signing secret. Implement this as an `axum` middleware layer before the event handler. Follow **Crate preferences (avoid reinventing wheels)** — use `hmac` + `sha2` (pure Rust) or an equivalent focused helper; do not roll a non-constant-time or body-re-parsed comparison.

```rust
// src/slack/events.rs — verify Slack request signature
// See: https://api.slack.com/authentication/verifying-requests-from-slack
// 1. Read X-Slack-Request-Timestamp header — reject if >5 min old
// 2. Compute HMAC-SHA256 of "v0:{timestamp}:{raw_body}" using signing secret
// 3. Compare with X-Slack-Signature header value (constant-time comparison)
```

### Delivery ingest, deduplication, and handler pipeline

Every **verified** `event_callback` is recorded before any business logic runs, so retries and filtered noise still leave an audit trail in redb.

| Step | Responsibility | Code |
|------|----------------|------|
| 1. **Ingest** | Append one row per HTTP delivery to `slack_ingest` (`SLACK_INGEST_TABLE`): `event_id`, `received_at_ms`, `retry_num` from `X-Slack-Retry-Num` (if present), inner `event.type`, initial outcome `Pending`. | `storage/slack_ingest::append` |
| 2. **Dedupe** | Atomically claim Slack’s top-level `event_id` in `META_TABLE` (`slack:ev:{event_id}`). If the claim fails, this delivery is a duplicate of an already-handled event: set ingest outcome to `DuplicateDelivery` and stop (no Claude / no Slack replies). | `storage/meta::try_claim_slack_delivery` |
| 3. **Filter** | Drop bot messages, subtyped events, unsupported `type`s, empty text. Release the dedupe claim when skipping so a later legitimate retry can run. Set ingest outcome (`FilteredBot`, `FilteredSubtype`, etc.). | `slack/handler.rs` |
| 4. **Action** | Classify intent → dispatch. On success: outcome `Processed`. On handler `Err`: **release** dedupe claim (so Slack can retry), outcome `Failed` (truncated error string). | `intent/*` |

**Do not** skip processing solely because `X-Slack-Retry-Num` is set; deduplication is keyed by `event_id`, not the retry header.

**Gap:** If the worker **panics** after ingest but before a terminal outcome, the row can remain `Pending` and the meta claim may strand. See **Open TODOs** below.

---

## Prompt design (JSON API)

User-derived text must **not** be interpolated into prompt templates with `format!`. Use typed payloads in `src/claude/payloads.rs` (`Serialize`) and **`serde_json`** for the strings sent to Claude. JSON is used (not TOML): it handles nested text, multiline Slack messages, and escaping automatically; TOML remains for human-edited config files only.

### Layout

| Piece | Where | Role |
|--------|--------|------|
| Session context | `SystemContextV1` (+ nested profile / clock) | `assistant_name`, user facts, London date/time, `api_version` |
| Task-specific user bodies | `IntentClassificationV1`, `MorningBriefingV1`, `FreeformQueryV1` | `task` discriminator + string fields (raw Slack text lives in JSON string values) |
| Static prose | `prompts::SYSTEM_CORE`, `SUPPLEMENT_*` | Fixed `&'static str`; append the right supplement to `system` per call type |

### Call shape

- **`system`**: `prompts::system_prompt_json(now)?` plus, when needed, `\n\n` + `SUPPLEMENT_INTENT_CLASSIFICATION` / `SUPPLEMENT_MORNING_BRIEFING` / `SUPPLEMENT_FREEFORM_QUERY`.
- **`user` (Claude “user” message content)**: a **single JSON object** from `intent_classification_user_json`, `morning_briefing_user_json`, or `freeform_query_user_json` (each returns `serde_json::Result<String>`).

Constants `PROMPT_API_VERSION` and `TASK_*` in `payloads.rs` identify the schema for future migrations.

### Behaviour notes (unchanged intent)

- Intent routing still resolves to exactly one of: `add_reminder`, `add_event`, `log_work`, `add_note`, `ask`; the supplement text tells the model to output only the label.
- Morning briefing still asks for summary, due/overdue reminders, and a prioritised todo list (max 7); the three context blobs are JSON fields on `MorningBriefingV1`.

---

## Scheduler Jobs

Implemented with `tokio-cron-scheduler`. All times are Europe/London (handle DST).

| Job | Schedule | Description |
|-----|----------|-------------|
| `morning_briefing` | `0 30 7 * * *` | Assemble context, call Claude, post to Slack |
| `reminder_check` | `0 * * * * *` | Scan reminders table, fire any due ones to Slack |
| `vault_sync` | `0 */5 * * * *` | Re-read vault Markdown files into redb |

The reminder_check job should use a redb range scan: iterate REMINDERS_TABLE where `due <= now` and `done == false`.

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

The vault sync parser should be tolerant of messy human edits. For structure (headings, lists, front matter), follow **Crate preferences (avoid reinventing wheels)** — use a Markdown parser and front-matter crate where they apply; fall back to simple heuristics only where the human-edited format is intentionally loose.

---

## Configuration

Load defaults from `config/default.toml` and merge **environment variables** (and optional overrides) via the **`config`** crate as described in **Crate preferences (avoid reinventing wheels)**.

### `.env` (secrets, never committed)

```env
ANTHROPIC_API_KEY=sk-ant-...
SLACK_BOT_TOKEN=xoxb-...
SLACK_SIGNING_SECRET=...
SLACK_CHANNEL_ID=C...      # The channel Mervyn posts briefings to
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
timezone = "Europe/London"

[storage]
db_path = "./data/mervyn.redb"
vault_path = "./data/vault"

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

Build and validate each layer before moving to the next. Each step should be independently testable.

1. **Storage layer** — `src/storage/`. Define tables, implement CRUD for all three record types (DRY with a macro or generic helper per **Crate preferences**). Write unit tests using a temp file path for the database.

2. **Storage read strategy (spike)** — Before vault sync and context assembly assume unbounded data, evaluate **paging / key-range scans / caps** in redb instead of loading whole tables with `list_all` → `Vec`. Decide patterns for: upcoming events window, pending reminders, recent worklog, reminder sweep, and any “dump for Claude” paths. Outcome should guide `context/assembler` and job implementations.

3. **Vault parser** — `src/vault/sync.rs`. Parse the three Markdown files into typed structs (Markdown + front-matter crates per **Crate preferences**). Unit test with fixture files. Upsert into redb.

4. **Claude client** — `src/claude/client.rs`. Implement the HTTP client and `complete()` method per **Crate preferences** (prefer a maintained client or generated types for the Messages API). Test with a hardcoded prompt against the real API.

5. **Context assembler** — `src/context/assembler.rs`. Implement `build_briefing_context()`. Test by printing the assembled string (use the read strategy from step 2).

6. **Prompt API** — `src/claude/payloads.rs` + `prompts.rs`. Wire callers to `system_prompt_json`, `*_user_json`, and `SUPPLEMENT_*` as in **Prompt design (JSON API)** above. Do not add new `format!`-based user interpolation.

7. **Scheduler** — `src/scheduler/jobs.rs`. Wire up cron jobs. Test morning briefing job end-to-end (storage → context → Claude → print output).

8. **Slack client** — `src/slack/client.rs`. Implement `post_message()`. Test by posting to the channel.

9. **Slack event receiver** — `src/api/mod.rs` + `src/slack/`. Axum endpoint, signature verification, envelope parsing, **ingest → dedupe → filter → action** (see *Delivery ingest, deduplication, and handler pipeline*), intent routing.

10. **Intent handlers** — `src/intent/`. Implement each handler. Wire everything together in `main.rs`.

11. **Docker** — Build image, test locally, deploy to VPS. Set up TLS termination (Caddy or nginx in front of port 3000).

---

## Open TODOs (engineering follow-ups)

Items below are **not** fully implemented yet; track them for production hardening.

- [ ] **`slack_ingest` retention** — Unbounded growth of `SLACK_INGEST_TABLE`. Add a policy (e.g. delete rows older than *N* days, or keep last *M* rows), run from a cron job or the existing scheduler, and document env/config knobs.
- [ ] **Stuck `Pending` ingest rows** — After a panic between `append` and `set_outcome`, or a crash after `try_claim_slack_delivery`, rows can stay `Pending` and meta claims can block retries. Options: wrap the worker in `catch_unwind` + outcome `Failed` / claim release; a periodic sweeper that flags old `Pending` rows; metrics / alerts on `Pending` count.
- [ ] **Operator visibility** — Read-only listing or export of recent ingest rows (by outcome, `event_id`, time range) for debugging without opening the raw redb file; optional small CLI or HTTP admin route (auth required).

---

## Notes for the Implementer

- Read **Crate preferences (avoid reinventing wheels)** before implementing Slack signing, Claude HTTP, vault parsing, recurrence, config layering, and storage CRUD patterns.
- Use `Arc<redb::Database>` everywhere — the database handle is shared across the scheduler, the HTTP handler, and the vault sync job.
- All async tasks communicate via the shared `Arc<Database>` and, if needed, a `tokio::sync::broadcast` channel for triggering immediate re-reads after vault writes.
- Keep Claude-facing text out of Slack/intent handlers: build `system` / user JSON via `src/claude/prompts.rs` and `payloads.rs` only. Handlers pass structured inputs into those APIs; do not add ad hoc `format!` with user-controlled text.
- The vault sync is one-directional for now: Obsidian → redb. Mervyn does not write back to the vault Markdown files in this initial version.
- Error handling: use `anyhow` for application-level errors, `thiserror` for library-level error types. Never `.unwrap()` in async task bodies — a panic in a spawned task is silent unless you explicitly handle the `JoinHandle`.
- Tracing: instrument every significant operation with `tracing::info!` / `tracing::debug!` spans. The Docker logs are your only observability.
- Slack: see **Open TODOs** for ingest retention, panic / stuck-`Pending` handling, and operator tooling.
