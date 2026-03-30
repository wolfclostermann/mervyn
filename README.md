# Mervyn

Personal AI assistant: Slack, Obsidian vault sync, `redb`, and Claude. Runs headlessly in Docker (deployment is often an **Oracle Cloud Ampere** ARM64 VPS—see the spec).

See [mervyn_project_spec.md](mervyn_project_spec.md) for architecture and implementation order.

**Prompts:** Claude inputs use a small JSON API — typed structs in `src/claude/payloads.rs` serialized with `serde_json`, plus static copy in `src/claude/prompts.rs` (`system_prompt_json`, `*_user_json`, `SUPPLEMENT_*`). See the spec section *Prompt design (JSON API)*.

## Quick start (dev)

```bash
cp .env.example .env
# Fill in secrets, then:
mkdir -p data/vault
cargo run
```

Health check: `GET http://localhost:3000/health`

## Docker

```bash
docker compose up --build
```
