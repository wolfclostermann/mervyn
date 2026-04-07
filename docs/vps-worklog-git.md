# VPS: Git auth for private `mervyn-worklog` (Docker)

Mervyn runs `git pull` in `[worklog_git].repo_path` on startup and on a schedule. If **mervyn-worklog** is **private**, `git` inside the container must authenticate.

## Recommended: HTTPS + fine-scoped PAT

1. In GitHub: **Settings → Developer settings → Personal access tokens** — create a token with **Contents: Read-only** (or read/write if you ever push from the VPS) scoped to **`wolfclostermann/mervyn-worklog`** only, if using fine-grained tokens.
2. On the VPS host, **do not** put the token in the image. Inject at runtime, e.g. Docker Compose **secret** or **env** only on the host.
3. In the mounted `/worklog` clone (or before first run), set the remote:
   ```bash
   git remote set-url origin https://x-access-token:${MERVYN_WORKLOG_GITHUB_PAT}@github.com/wolfclostermann/mervyn-worklog.git
   ```
   Or use `git config credential.helper` with a file mounted read-only from a secret.
4. Add **`MERVYN_WORKLOG_GITHUB_PAT`** (or your chosen name) to **`.env`** on the server (gitignored); reference it in Compose with **`env_file`** or **`environment`** only if you expand it into a one-shot init — avoid printing it in logs.

**Security:** Rotate the PAT if leaked; prefer a **machine user** or **deploy key** (SSH) if you want repo-scoped keys without a full PAT.

## Alternative: deploy key (SSH)

Mount an SSH private key and `known_hosts` into the container; set `GIT_SSH_COMMAND` or `~/.ssh/config` for the `git` user in the container. Read-only deploy key on the repo is a good fit.

## Checklist

- [ ] Token or key created with minimum scope  
- [ ] Remote URL or credential helper updated **inside** `/worklog` on the VPS  
- [ ] `.env` / secrets never committed  
- [ ] After deploy: confirm logs show **`worklog git pull ok`** (or fix auth until they do)
