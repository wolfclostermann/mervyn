# Mervyn

Personal AI assistant: Slack Events API, Obsidian vault sync into `redb`, scheduled jobs, and Claude. Runs headlessly in Docker (deployment is often an **Oracle Cloud Ampere** ARM64 VPS—see the spec).

See [mervyn_project_spec.md](mervyn_project_spec.md) for architecture, Slack ingest/dedupe pipeline, and implementation notes.

**Runtime:** On startup, the binary opens `redb`, builds shared [`AppState`](src/state.rs) (config, secrets, Claude + Slack clients, DB, vault path), spawns the **cron scheduler** ([`scheduler/jobs.rs`](src/scheduler/jobs.rs)), and starts a **debounced vault file watcher** ([`vault/watcher.rs`](src/vault/watcher.rs)) so Markdown edits sync to the database without waiting for the periodic vault job.

**Claude:** The Messages API is called with **`reqwest`** ( **`rustls-tls`**, default features off) and small request/response types in [`claude/client.rs`](src/claude/client.rs). Prompt bodies still use the JSON API — typed structs in [`claude/payloads.rs`](src/claude/payloads.rs) and static copy in [`claude/prompts.rs`](src/claude/prompts.rs). See the spec *Prompt design (JSON API)*.

## Quick start (dev)

```bash
cp .env.example .env
```

Edit `.env` with all required variables (`ANTHROPIC_API_KEY`, `SLACK_BOT_TOKEN`, `SLACK_SIGNING_SECRET`, **`SLACK_CHANNEL_ID`** for briefings and reminder posts). Optional: override `config/default.toml` with `MERVYN__` env vars (see `.env.example`).

```bash
mkdir -p data/vault
cargo run
```

Default listen port comes from `config/default.toml` (`[server] port`, usually **3000**). Health check: `GET http://localhost:3000/health`

## Docker

```bash
docker compose up --build
```
