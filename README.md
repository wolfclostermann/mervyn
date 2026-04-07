# Mervyn

Personal AI assistant: Slack Events API, Obsidian vault sync into `redb`, scheduled jobs, and Claude. Runs headlessly in Docker (deployment is often an **Oracle Cloud Ampere** ARM64 VPS—see the spec).

See [mervyn_project_spec.md](mervyn_project_spec.md) for architecture, Slack ingest/dedupe pipeline, and implementation notes.

**Runtime:** On startup, the binary opens `redb`, builds shared [`AppState`](src/state.rs) (config, secrets, Claude + Slack clients, DB, vault path), spawns the **cron scheduler** ([`scheduler/jobs.rs`](src/scheduler/jobs.rs)), and starts a **debounced vault file watcher** ([`vault/watcher.rs`](src/vault/watcher.rs)) so Markdown edits sync to the database without waiting for the periodic vault job.

**Claude:** The Messages API is called with **`reqwest`** ( **`rustls-tls`**, default features off) and small request/response types in [`claude/client.rs`](src/claude/client.rs). Prompt bodies still use the JSON API — typed structs in [`claude/payloads.rs`](src/claude/payloads.rs) and static copy in [`claude/prompts.rs`](src/claude/prompts.rs). See the spec *Prompt design (JSON API)*.

## Claude (Anthropic)

Mervyn calls the **Claude Messages API** using an API key (no OAuth flow in the app).

1. Sign in at [Anthropic Console](https://console.anthropic.com/).
2. Open **API keys** and create a key with a name you recognise (e.g. `mervyn-prod`).
3. Copy the key into `.env` as **`ANTHROPIC_API_KEY`** (see [`.env.example`](.env.example)).

The default model and token limit are in [`config/default.toml`](config/default.toml) under `[claude]`; you can override with `MERVYN__CLAUDE__MODEL` and `MERVYN__CLAUDE__MAX_TOKENS` if needed.

## Slack app (Events API)

Slack delivers events over HTTPS to **`POST /slack/events`** on your Mervyn host. The app verifies each request with the **signing secret** ([Verifying requests from Slack](https://api.slack.com/authentication/verifying-requests-from-slack)).

### 1. Create the app

1. Go to [Your Apps](https://api.slack.com/apps) and choose **Create New App** (from scratch is fine).
2. Pick your workspace and a name (e.g. Mervyn).

### 2. Bot token scopes

Under **OAuth & Permissions** → **Scopes** → **Bot Token Scopes**, add at least:

| Scope | Why |
| ----- | --- |
| `chat:write` | Post briefings, reminders, and replies (`chat.postMessage`) |
| `channels:history` | Read message text in **public** channels (for `message.channels` events) |
| `groups:history` | Read message text in **private** channels the bot is in (for `message.groups` events) — **required** if Mervyn lives in a private channel |
| `app_mentions:read` | Receive `app_mention` events |

DMs would need `im:history` and `message.im` if you ever wire that; not required for channel use.

### 3. Event subscriptions

1. Under **Event Subscriptions**, turn **Enable Events** on.
2. Set **Request URL** to `https://<your-public-host>/slack/events` (must be HTTPS and reachable from Slack’s servers). For local development, use a tunnel ([ngrok](https://ngrok.com/), [Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-apps/), etc.) so that URL points at your machine’s Mervyn port (e.g. `https://abc.ngrok.io/slack/events`).
3. Slack will send a URL verification challenge; Mervyn answers it when the route is wired correctly.
4. Under **Subscribe to bot events**, add:

   - `message.channels` — ordinary messages in **public** channels the bot is in  
   - `message.groups` — ordinary messages in **private** channels the bot is in (without this, only `@mentions` reach Mervyn in private channels)  
   - `app_mention` — when someone `@mentions` the bot  

After changing scopes or events, open **OAuth & Permissions** and **Reinstall to Workspace** so the bot token picks up new permissions.

### 4. Install and copy secrets

1. **Install to Workspace** (still under **OAuth & Permissions**).
2. Copy **Bot User OAuth Token** (`xoxb-...`) → **`SLACK_BOT_TOKEN`** in `.env`.
3. Under **Basic Information** → **App Credentials**, copy **Signing Secret** → **`SLACK_SIGNING_SECRET`** in `.env`.

### 5. Channel ID (`SLACK_CHANNEL_ID`)

Scheduled briefings and reminders are posted to a single channel. Invite the bot to that channel, then set **`SLACK_CHANNEL_ID`** to that channel’s ID (from **About** / channel details or the channel URL — public is often `C…`, private is often `G…`). Typical ways to get it:

- In the Slack desktop app: open the channel → channel name → **About** / details, or copy a link to the channel and take the ID from the URL.
- Or call [`conversations.list`](https://api.slack.com/methods/conversations.list) with your bot token and find the channel.

## Quick start (dev)

After [Claude](#claude-anthropic) and [Slack](#slack-app-events-api) setup:

```bash
cp .env.example .env
```

Edit `.env` with all required variables (`ANTHROPIC_API_KEY`, `SLACK_BOT_TOKEN`, `SLACK_SIGNING_SECRET`, **`SLACK_CHANNEL_ID`** for briefings and reminder posts). Optional: override `config/default.toml` with `MERVYN__` env vars (see `.env.example`).

```bash
mkdir -p data/vault
cargo run
```

Default listen port comes from `config/default.toml` (`[server] port`, usually **3000**). Health check: `GET http://localhost:3000/health`

Optional **operator** JSON: set `MERVYN_ADMIN_TOKEN` in `.env`, then `GET /admin/slack-ingest` with header `Authorization: Bearer <token>` and query params `limit`, `since_ms`, `until_ms`, `outcome`, `event_id` (see spec).

Optional **built-in ngrok tunnel** (local dev): set `NGROK_AUTHTOKEN` and `MERVYN__NGROK__ENABLED=true` in `.env`, then run `cargo run`. Mervyn logs a public URL and the exact Slack Events URL to paste into Slack app settings. You can also set `MERVYN__NGROK__DOMAIN=<reserved-domain>` if you use a reserved ngrok domain.

## Usage

Interaction is the same whether you run with **`cargo run`** or **Docker Compose**: Slack and the vault are the main surfaces; HTTP is for health, Slack delivery, and optional admin.

### Slack

1. **Invite the bot** into the public channel you care about (the one whose ID you set as `SLACK_CHANNEL_ID`, and any other channels you want it to read from).
2. **Send a normal message** in that channel, or **@mention the bot**. In **public** channels that requires `message.channels`; in **private** channels it requires **`message.groups`** plus `groups:history` (see above). Mervyn ignores its own bot messages and most message subtypes.
3. **Natural language in, structured action out:** your text is sent to Claude for **intent classification**, then one handler runs and Mervyn **replies in Slack** (in the same channel; replies stay in the thread when Slack sends a thread timestamp). If classification fails, the message is treated as a plain **question** (`ask`).

Roughly what each intent does (you do **not** type these labels yourself—describe what you want in ordinary sentences):

| Intent (internal) | What it does |
| ----------------- | ------------ |
| **Reminder** | Stores a reminder in `redb`. If the message contains an ISO date `YYYY-MM-DD`, that day is used (default time 09:00 UTC); otherwise due time defaults to about **tomorrow** at 09:00 UTC. Due reminders are **posted to `SLACK_CHANNEL_ID`** on a schedule (see below). |
| **Event** | Saves a calendar-style **event** in `redb` with a default start around **24 hours** ahead; refine times and detail in Obsidian (`events.md`) if needed. |
| **Work log** | Appends a **worklog** entry with the current UTC timestamp. |
| **Note** | Does **not** write files: replies with guidance to put durable notes under **`data/vault/notes/`** in your Obsidian layout. |
| **Ask** | Builds **context** from `redb` plus vault Markdown and asks Claude for an answer, then returns that text in Slack. |

### Obsidian vault (`data/vault`)

Treat **`data/vault`** as the human-readable layer (e.g. synced Obsidian folder). Expected top-level files include **`worklog.md`**, **`reminders.md`**, and **`events.md`**, plus a **`notes/`** tree—see [mervyn_project_spec.md](mervyn_project_spec.md) for Markdown/front-matter behaviour. **Edits on disk** are picked up by a **debounced file watcher** and by a periodic **vault sync** job so the database stays aligned with what you typed in the editor.

### Background jobs (defaults)

Schedules and timezone come from [`config/default.toml`](config/default.toml) (`[scheduler]`); override with `MERVYN__SCHEDULER__…` if needed.

- **Morning briefing** — generated from vault + `redb` and **posted to `SLACK_CHANNEL_ID`** (default cron **07:30** in **`Europe/London`**).
- **Reminder check** — runs **every minute**; posts due reminders to **`SLACK_CHANNEL_ID`** and advances or completes them.
- **Vault sync** — periodic import from Markdown into `redb` (default **every five minutes**), in addition to the live watcher.
- **Slack ingest maintenance** — prunes old ingest rows and sweeps stuck “pending” deliveries (housekeeping, not user-facing).

### HTTP endpoints

| Method / path | Purpose |
| ------------- | ------- |
| `GET /health` | Liveness; responds with `ok`. |
| `POST /slack/events` | Slack Events API (signing secret verification, URL challenge, event callbacks). |
| `GET /admin/slack-ingest` | Optional: JSON view of ingest log when `MERVYN_ADMIN_TOKEN` is set (Bearer auth). |

## Docker Compose

Compose loads secrets and optional `MERVYN__*` overrides from a **`.env` file next to `docker-compose.yml`** ([`env_file` in `docker-compose.yml`](docker-compose.yml)). That is the same file as in [Quick start](#quick-start-dev): copy [`.env.example`](.env.example) to `.env`, fill in the required variables, and **do not commit `.env`** (it is gitignored).

**Before first run**

1. **`.env`** — populated with real `ANTHROPIC_API_KEY`, `SLACK_BOT_TOKEN`, `SLACK_SIGNING_SECRET`, and `SLACK_CHANNEL_ID` (and any optional keys from `.env.example`). Compose injects these into the container; there is no separate “Docker secrets” step unless you choose to add one yourself.
2. **`data/`** — create the vault directory on the host so the bind mount exists and is writable:

   ```bash
   mkdir -p data/vault
   ```

   [`docker-compose.yml`](docker-compose.yml) mounts **`./data` → `/app/data`**, so `mervyn.redb` and vault Markdown live on your machine and survive container restarts. The database file is created on first startup if missing.

**Already in the image** — [`config/default.toml`](config/default.toml) is copied into the image at build time (`Dockerfile`), so you do not need to mount `config/` for a default run. Override behaviour with `MERVYN__…` environment variables in `.env` if you need different ports, paths, or schedules.

**Slack** — Your Events API Request URL must reach the HTTP server that ends at this container (for example `https://<tunnel-or-host>/slack/events` with port **3000** published as in compose, or a reverse proxy in front).

```bash
docker compose up --build
```

**Podman:** `podman compose up --build` (or `podman-compose up --build`) from the same directory, with the same `.env` and `data/vault` steps.
