# VPS: Git auth for private `mervyn-worklog` (Docker)

Mervyn runs `git pull` in `[worklog_git].repo_path` on startup and on a schedule. If **mervyn-worklog** is **private**, `git` inside the container must authenticate.

## Recommended: `MERVYN_WORKLOG_GITHUB_PAT` in `.env` (Compose)

Mervyn reads **`MERVYN_WORKLOG_GITHUB_PAT`** from the environment (e.g. `env_file: .env` in Compose). When set, it configures Git to send **`Authorization: basic …`** (username `x-access-token`, password = the secret) for `https://github.com/` during `git pull` only—no need to embed the token in `git remote` URLs inside the mount.

- **From `gh` (wolfclostermann):** `gh auth token -h github.com -u wolfclostermann` prints the active GitHub CLI OAuth token (`repo` scope), which works for private repo pulls until you revoke the `gh` session.
- **Dedicated PAT:** create a fine-grained or classic PAT in GitHub Settings with **Contents: Read** on `mervyn-worklog` only, and paste that value instead.

## Alternative: HTTPS remote URL with token

1. In GitHub: **Settings → Developer settings → Personal access tokens** — create a token with **Contents: Read-only** (or read/write if you ever push from the VPS) scoped to **`wolfclostermann/mervyn-worklog`** only, if using fine-grained tokens.
2. On the VPS host, **do not** put the token in the image. Inject at runtime, e.g. Docker Compose **secret** or **env** only on the host.
3. In the mounted `/worklog` clone (or before first run), set the remote:
   ```bash
   git remote set-url origin https://x-access-token:${MERVYN_WORKLOG_GITHUB_PAT}@github.com/wolfclostermann/mervyn-worklog.git
   ```
   Or use `git config credential.helper` with a file mounted read-only from a secret.
4. Ensure Compose **`env_file`** (or shell) expands **`MERVYN_WORKLOG_GITHUB_PAT`** only for that `git remote set-url` command—avoid printing it in logs.

**Security:** Rotate the token if leaked; prefer a **deploy key** (SSH) if you want repo-scoped keys without tying pulls to your user account.

**Note:** If you use the **recommended** `.env` bearer variable above, you can keep a normal `https://github.com/...` remote in the clone; Mervyn supplies auth for `github.com` pulls.

## Alternative: deploy key (SSH)

Mount an SSH private key and `known_hosts` into the container; set `GIT_SSH_COMMAND` or `~/.ssh/config` for the `git` user in the container. Read-only deploy key on the repo is a good fit.

## Checklist

- [ ] Token or key created with minimum scope  
- [ ] Either **`MERVYN_WORKLOG_GITHUB_PAT`** in `.env`, or remote URL / credential helper updated **inside** `/worklog` on the VPS  
- [ ] `.env` / secrets never committed  
- [ ] After deploy: confirm logs show **`worklog git pull ok`** (or fix auth until they do)
