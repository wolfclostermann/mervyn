# Mervyn

Personal AI assistant: Telegram bot (long polling), two-way Obsidian vault sync with `redb`, scheduled jobs, and Claude. Runs headlessly in Docker (deployment is often an **Oracle Cloud Ampere** ARM64 VPS—see the spec).

See [mervyn_project_spec.md](mervyn_project_spec.md) for architecture, the message ingest/dedupe pipeline, and implementation notes.

**Runtime:** On startup, the binary opens `redb`, builds shared [`AppState`](src/state.rs) (config, secrets, Claude + Telegram clients, DB, vault path), spawns the **cron scheduler** ([`scheduler/jobs.rs`](src/scheduler/jobs.rs)), starts a **debounced vault file watcher** ([`vault/watcher.rs`](src/vault/watcher.rs)) so Markdown edits sync to the database without waiting for the periodic vault job, and starts the **Telegram long-poll loop** ([`telegram/poller.rs`](src/telegram/poller.rs)) alongside the local HTTP server.

**Claude:** The Messages API is called with **`reqwest`** ( **`rustls-tls`**, default features off) and small request/response types in [`claude/client.rs`](src/claude/client.rs). Prompt bodies still use the JSON API — typed structs in [`claude/payloads.rs`](src/claude/payloads.rs) and static copy in [`claude/prompts.rs`](src/claude/prompts.rs). See the spec *Prompt design (JSON API)*.

## Claude (Anthropic)

Mervyn calls the **Claude Messages API** using an API key (no OAuth flow in the app).

1. Sign in at [Anthropic Console](https://console.anthropic.com/).
2. Open **API keys** and create a key with a name you recognise (e.g. `mervyn-prod`).
3. Copy the key into `.env` as **`ANTHROPIC_API_KEY`** (see [`.env.example`](.env.example)).

The default model and token limit are in [`config/default.toml`](config/default.toml) under `[claude]`; you can override with `MERVYN__CLAUDE__MODEL` and `MERVYN__CLAUDE__MAX_TOKENS` if needed.

## Telegram bot

Mervyn talks to Telegram over **long polling**: it calls [`getUpdates`](https://core.telegram.org/bots/api#getupdates) on `api.telegram.org`, waits up to 30s for new messages, and replies with `sendMessage` ([`telegram/client.rs`](src/telegram/client.rs), [`telegram/poller.rs`](src/telegram/poller.rs)). **Every connection is outbound.** There is no webhook, no public endpoint, no tunnel, no reserved domain and no request signature to verify — nothing on the internet needs to reach your host, and the HTTP server stays private.

> ### ⚠️ The chat allowlist is the only thing keeping the bot private
>
> Long polling has no transport-level authentication. Unlike a Slack signing secret, nothing stops a stranger who discovers your bot's username from messaging it, and Telegram will deliver those messages to you. **`TELEGRAM_CHAT_ID`** is an allowlist of **exactly one chat**, checked in [`telegram/handler.rs`](src/telegram/handler.rs) **before** anything is written to the database and **before** any Claude call — so an unknown sender cannot grow your database, spend your Anthropic quota, or see a reply. Unauthorised updates are logged at `warn` and dropped.
>
> Consequences worth taking seriously: set the id correctly (a wrong id means the bot answers someone else's chat, not yours), keep **`TELEGRAM_BOT_TOKEN`** secret — it sits in the URL of every API call and anyone holding it controls the bot — and rotate it via [@BotFather](https://t.me/BotFather) if it leaks. A `TELEGRAM_CHAT_ID` of `0` is rejected at startup so an empty or misconfigured value cannot pair with a chat-less update and look like a match.

### 1. Create the bot

1. Open Telegram — **Desktop, web, iOS or Android, all on one account**; there is no workspace to join or pay for — and start a chat with **[@BotFather](https://t.me/BotFather)**.
2. Send **`/newbot`** and follow the prompts: a display name, then a username ending in `bot`.
3. BotFather replies with the **HTTP API token** (`123456789:AA…`). Copy it into `.env` as **`TELEGRAM_BOT_TOKEN`** (see [`.env.example`](.env.example)).

Optional, under `/mybots` → your bot → **Bot Settings**: give it a description, and note **Group Privacy**. It is *on* by default, which means that in a **group** the bot only receives commands and messages that mention it. If you intend to talk to Mervyn in a group rather than in a direct chat, turn Group Privacy **off** so ordinary messages reach it (then re-add the bot to the group for the change to take effect). A direct chat with the bot is unaffected.

### 2. Get the chat id (`TELEGRAM_CHAT_ID`)

Do this **before** starting Mervyn — two long pollers on the same token compete for updates, so `getUpdates` by hand will steal messages from a running instance (and vice versa).

1. Open the chat Mervyn should live in — a direct chat with your bot, or a group you added it to — and **send it any message** (`/start` is fine).
2. Ask Telegram for the pending updates:

   ```bash
   curl "https://api.telegram.org/bot<TELEGRAM_BOT_TOKEN>/getUpdates"
   ```

3. Read **`result[].message.chat.id`** from the JSON and put it in `.env` as **`TELEGRAM_CHAT_ID`**. It is a plain number: **positive** for a direct chat, **negative** for a group or supergroup. No quotes, no `@username`.

That same chat is where the **morning briefing** and **due reminders** are posted. Mervyn also sends a short greeting on startup and a goodbye on shutdown, which is the quickest way to confirm the token and the id are both right; `getMe` is called at startup too and logs a warning if the token is bad.

### 3. How replies look

Replies are sent as **plain text with no `parse_mode`** — Claude's output is Markdown-ish and appointment reminders embed curly quotes, every one of which MarkdownV2 would demand be escaped. Anything longer than Telegram's **4096-character** limit is split across several messages, breaking on a newline where one is available ([`telegram/client.rs`](src/telegram/client.rs)). The client also honours Telegram's `retry_after` on HTTP 429 (capped at 60s), which matters when a burst of due reminders is posted in one pass.

## Quick start (dev)

After [Claude](#claude-anthropic) and [Telegram](#telegram-bot) setup:

```bash
cp .env.example .env
```

Edit `.env` with all required variables (`ANTHROPIC_API_KEY`, `TELEGRAM_BOT_TOKEN`, **`TELEGRAM_CHAT_ID`** — the allowlisted chat, and where briefings and reminder posts go). Optional: override `config/default.toml` with `MERVYN__` env vars (see `.env.example`).

```bash
mkdir -p data/vault
cargo run
```

Default listen port comes from `config/default.toml` (`[server] port`, usually **3000**). Health check: `GET http://localhost:3000/health`

Optional **operator** JSON: set `MERVYN_ADMIN_TOKEN` in `.env`, then `GET /admin/message-ingest` with header `Authorization: Bearer <token>` and query params `limit`, `since_ms`, `until_ms`, `outcome`, `event_id` (see spec).

**No inbound setup is needed.** The bot works as soon as the token and chat id are right: no tunnel, no reverse proxy, no TLS certificate, no DNS name and no firewall rule for the app port. The HTTP server is there for `/health` and the optional admin route only.

## Usage

Interaction is the same whether you run with **`cargo run`** or **Docker Compose**: Telegram and the vault are the main surfaces; HTTP is only for health and the optional admin route.

### Telegram

1. **Open the chat** whose id you set as `TELEGRAM_CHAT_ID` — a direct chat with the bot, or the group you added it to (in a group, see the Group Privacy note above).
2. **Send a normal message.** Mervyn ignores messages from bots and non-text updates (stickers, photos, service events), and anything from another chat is dropped by the allowlist before it is even recorded.
3. **Natural language in, structured action out:** your text is sent to Claude for **intent classification**, then one handler runs and Mervyn **replies in the same chat**. If classification fails, the message is treated as a plain **question** (`ask`).

Roughly what each intent does (you do **not** type these labels yourself—describe what you want in ordinary sentences):

| Intent (internal) | What it does |
| ----------------- | ------------ |
| **Reminder** | Stores a reminder in `redb`. Understands a time written as `5pm`, `6 am`, `7p.m.`, `17:00` or `5:30am`, and an ISO date `YYYY-MM-DD`, `today` or `tomorrow`. With no time it defaults to 09:00; with nothing recognised at all, about **tomorrow** at 09:00 UTC. Weekday names (`on Weds`) are **not** understood yet. Due reminders are **posted to `TELEGRAM_CHAT_ID`** on a schedule (see below). |
| **Event** | Saves a calendar-style **event** in `redb`, and (with write-back on) writes it into `events.md`; refine times and detail in Obsidian either way. |
| **Work log** | Appends a **worklog** entry with the current UTC timestamp. |
| **Note** | Parses one or more **todo** lines and stores them in **`redb`** (open until marked done), and with write-back on mirrors them to `todos.md` as checkboxes. They appear in **morning briefing** context and **ask** context. For long-form writing you can still use **`data/vault/notes/`** in Obsidian. |
| **Complete todo** | Loads **open** todos, uses Claude to match your wording (or numeric id) to rows, then sets **`done`** in **`redb`** — which ticks the box in `todos.md`. Ticking it in Obsidian works too. |
| **Ask** | Builds **context** from `redb` plus vault Markdown and asks Claude for an answer, then returns that text in Telegram (split into 4096-character messages if it is long). |

### Obsidian vault (`data/vault`)

Treat **`data/vault`** as the human-readable layer (e.g. a synced Obsidian folder). Top-level files
are **`reminders.md`**, **`events.md`**, **`todos.md`** and **`worklog.md`**, plus a **`notes/`**
tree — see [mervyn_project_spec.md](mervyn_project_spec.md) for Markdown/front-matter behaviour.
**Edits on disk** are picked up by a **debounced file watcher** and by a periodic **vault sync**
job.

Sync is **two-way** for reminders, events and todos: rows added from chat are written into the
vault, and edits made in Obsidian reach the database. Each item carries its row id in an HTML
comment — `- [ ] Pay tax — due 2026-11-15 09:00 <!--mv:1a2b-->` — which Obsidian hides in reading
view and which is what lets an edited line keep its identity. Leave it in place; a line without one
is treated as new and given one on the next sync.

Times are **local wall-clock** in `scheduler.timezone`, written without an offset, and resolved for
the date in question — writing `2026-11-15 09:00` in August still means 09:00 GMT that morning. A
bare date means local noon. Event headings take `## YYYY-MM-DD [HH:MM[–HH:MM]] — Title`.

**Write-back is off by default.** Set `MERVYN__VAULT__WRITE_BACK_ENABLED=true` (or `[vault]` in
[`config/default.toml`](config/default.toml)) to let Mervyn modify the files; until then it only
reads them. The first write of each run copies the managed files to a timestamped sibling of the
vault. **`worklog.md` is never written** — it is a symlink into the worklog clone, and touching
that working tree would break the scheduled `git pull --ff-only`.

Deletions work in both directions: removing a line deletes the row, and deleting an event from chat
removes its section. If the whole file goes missing — a moved vault, an unmounted volume — nothing
is deleted. See [docs/two-way-vault-sync.md](docs/two-way-vault-sync.md) for the merge rules.

### Getting the vault onto a laptop and a phone

The vault directory is the working tree of a private git repo. With `[vault_git]` enabled, each
cycle pulls, reconciles, then commits and pushes what changed; Obsidian Git does the same at the
other end, on desktop and mobile. Obsidian Sync cannot do this job — it runs inside the Obsidian
app, and there is no headless client to run on the server.

```sh
# once, on the box that holds the vault
MERVYN_VAULT_GITHUB_PAT=… ./scripts/vault-git-init.sh ~/mervyn/data/vault \
    https://github.com/you/mervyn-vault.git main
```

Then set `MERVYN__VAULT_GIT__ENABLED=true` and `MERVYN_VAULT_GITHUB_PAT` (a token with **write**
access). The **worklog lives in this repo too** — `worklog.md` is an ordinary file in the vault,
not a symlink into a separate clone, and the old `[worklog_git]` section is gone.

If an edit on your phone and an edit by Mervyn land on nearby lines between two syncs, the rebase
will conflict. Mervyn aborts it, stops writing, and tells you in chat; resolve it in the clone and
it picks up again.

### Background jobs (defaults)

Schedules and timezone come from [`config/default.toml`](config/default.toml) (`[scheduler]`); override with `MERVYN__SCHEDULER__…` if needed.

- **Morning briefing** — generated from vault + `redb` and **posted to `TELEGRAM_CHAT_ID`** (default cron **07:30** in **`Europe/London`**).
- **Reminder check** — runs **every minute**; posts due reminders to **`TELEGRAM_CHAT_ID`** and advances or completes them.
- **Vault sync** — periodic reconcile between Markdown and `redb` (default **every five minutes**), in addition to the live watcher.
- **Vault git sync** — when `[vault_git]` is on, pull → reconcile → commit → push on the same cadence, carrying the vault to your other devices.
- **Message ingest maintenance** — prunes old `message_ingest` rows and sweeps stuck “pending” deliveries (housekeeping, not user-facing).

### HTTP endpoints

The server binds locally so the process has a liveness probe and a clean shutdown path. Telegram is reached by **outbound** long polling, so neither route needs to be published to the internet.

| Method / path | Purpose |
| ------------- | ------- |
| `GET /health` | Liveness; responds with `ok`. |
| `GET /admin/message-ingest` | JSON view of the inbound delivery log. |
| `GET /admin/events` | Every stored event, earliest first. |
| `GET /admin/reminders` | Stored reminders (`?include_done=true` for closed ones). |
| `GET /admin/todos` | Stored todos, each with the `list_number` chat shows, so "todo 2" maps to a row. |
| `GET /admin/vault` | Where the vault and the database disagree, and why — see below. |

The `/admin/*` routes exist only when **`MERVYN_ADMIN_TOKEN`** is set, take `Authorization: Bearer
<token>`, and are read-only. They report what is in the tables and nothing more — unlike asking
Mervyn, which answers through Claude and is the thing you are usually trying to check. Row keys
travel as lower-case hex, matching the vault markers, because a `u64` key does not survive a JSON
number intact.

There is no public URL, so reach them over SSH:

```sh
ssh opc@<host> 'curl -s -H "Authorization: Bearer $TOKEN" localhost:3000/admin/vault' | jq
```

`/admin/vault` answers the question neither side can answer alone: for each managed file it lists
rows that exist only in the database, lines that exist only in the file, and — by comparing both
against the snapshot they last agreed on — **which side moved**. It also shows unmarked lines,
pending tombstones (one that never clears means a write is failing), and whether the git working
tree has uncommitted changes.

## GitHub account

This repo lives under, and should stay under, the personal `wolfclostermann` GitHub account —
not any work-linked account. Two things enforce that:

- **git push/pull** already route through the personal identity via an SSH host alias
  (`git@me.github.com` → personal key, see `~/.ssh/config` / `~/.gitconfig` `insteadOf` rule).
  Commit authorship in this repo is set locally (`git config user.email`) to the personal
  account's GitHub noreply address, independent of your global git identity.
- **`gh` CLI** (`gh pr create`, `gh issue`, `gh api`, etc.) has no per-directory account
  awareness — it uses whichever account is globally active. To scope it to personal here
  without touching your global `gh auth switch` state, this repo has an `.envrc`
  ([direnv](https://direnv.net)) that exports `GH_TOKEN` for the personal account while you're
  inside this directory. Run `direnv allow` once after installing direnv and logging in with
  `gh auth login -h github.com -u wolfclostermann`. Without direnv, run the same export
  manually before `gh` commands in this repo:
  `export GH_TOKEN=$(gh auth token -h github.com -u wolfclostermann)`.

## Docker Compose

Compose loads secrets and optional `MERVYN__*` overrides from a **`.env` file next to `docker-compose.yml`** ([`env_file` in `docker-compose.yml`](docker-compose.yml)). That is the same file as in [Quick start](#quick-start-dev): copy [`.env.example`](.env.example) to `.env`, fill in the required variables, and **do not commit `.env`** (it is gitignored).

**Before first run**

1. **`.env`** — populated with real `ANTHROPIC_API_KEY`, `TELEGRAM_BOT_TOKEN`, and `TELEGRAM_CHAT_ID` (and any optional keys from `.env.example`). Compose injects these into the container; there is no separate “Docker secrets” step unless you choose to add one yourself.
2. **`data/`** — create the vault directory on the host so the bind mount exists and is writable:

   ```bash
   mkdir -p data/vault
   ```

   [`docker-compose.yml`](docker-compose.yml) mounts **`./data` → `/app/data`**, so `mervyn.redb` and vault Markdown live on your machine and survive container restarts. The database file is created on first startup if missing.

**Already in the image** — [`config/default.toml`](config/default.toml) is copied into the image at build time (`Dockerfile`), so you do not need to mount `config/` for a default run. Override behaviour with `MERVYN__…` environment variables in `.env` if you need different ports, paths, or schedules.

**Networking** — the container needs **outbound HTTPS only** (`api.telegram.org` and `api.anthropic.com`). Nothing connects in, so no inbound firewall rule, published port, tunnel or TLS termination is required for the bot to work. Accordingly the default [`docker-compose.yml`](docker-compose.yml) does **not** publish port **3000**; if you want `curl` / browser access to `/health` from your machine, use [`docker-compose.local.yml`](docker-compose.local.yml), which binds it to `127.0.0.1` only: `docker compose -f docker-compose.yml -f docker-compose.local.yml up --build`.

```bash
docker compose up --build
```

**Podman:** `podman compose up --build` (or `podman-compose up --build`) from the same directory, with the same `.env` and `data/vault` steps.
