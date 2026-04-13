# Mervyn worklog sync (no Mervyn code changes)

Global **git `post-commit`** hook: after you commit in a project repo, append one bullet to a shared **`worklog.md`** (Mervyn’s vault format: `## YYYY-MM-DD` sections and `-` list items), then **commit and push** from the worklog git repo.

On the machine where **Mervyn** runs, **pull** that repo on a timer so `vault/worklog.md` stays current.

---

## 1. Create the worklog repository

On GitHub (or elsewhere), create a repo that contains `worklog.md`. Minimal starter:

```markdown
# Worklog

## 2026-04-07
- **example** `0000000` seed entry (replace or delete)
```

Clone it somewhere stable, e.g. `~/Code/Play/mervyn-worklog`.

---

## 2. Point Mervyn at that file

Pick one:

- **Symlink** into your Mervyn vault. Prefer an **absolute** target so tools (e.g. Cursor) that resolve links from the workspace root still find the file; relative `../../../mervyn-worklog/worklog.md` is correct for the shell but can break in editors.  
  `ln -sf /Users/you/Code/Play/mervyn-worklog/worklog.md /path/to/mervyn/data/vault/worklog.md`  
  If you use a relative link from **`mervyn/data/vault`**, it must be **three** `..` segments, then **`mervyn-worklog/worklog.md`** (not two `..`).
- Or keep **only** `worklog.md` in the worklog repo and put the rest of the vault (reminders, events, notes) beside it on disk as you already do.

Mervyn only needs `vault_path/worklog.md` to exist and match the [worklog format](../../mervyn_project_spec.md) (`## YYYY-MM-DD` + bullets).

---

## 3. Install hooks from your dotfiles

Copy or symlink the **`hooks/`** directory from this folder into your dotfiles tree, e.g.:

```text
~/dotfiles/git/hooks/post-commit
~/dotfiles/git/hooks/append-worklog-line.py
```

Make them executable:

```bash
chmod +x ~/dotfiles/git/hooks/post-commit ~/dotfiles/git/hooks/append-worklog-line.py
```

Point Git at that directory (one-time per machine / stored in dotfiles bootstrap):

```bash
git config --global core.hooksPath ~/dotfiles/git/hooks
```

`core.hooksPath` is **not** copied when you clone dotfiles; re-run the `git config` command on each new machine (or script it in your dotfiles install). The hooks themselves **are** just files in the synced repo.

---

## 4. Config: `~/.config/mervyn-worklog/env`

```bash
mkdir -p ~/.config/mervyn-worklog
cp config/env.example ~/.config/mervyn-worklog/env
# edit paths
```

Use **`export`** for every variable you set so `python3` sees optional flags (see `env.example`). Set **`MERVYN_WORKLOG_PROJECT_PREFIXES`** to **`$HOME/Code`** (or similar) if you want every repo under `~/Code` to append to the worklog; narrower prefixes skip other trees.

---

## 5. Pull on the Mervyn host (built-in scheduler)

Mervyn can run **`git pull --ff-only`** on the same Tokio cron as vault sync—**only while the process is running** (no OS cron needed).

In `config/default.toml` (or env overrides):

```toml
[worklog_git]
enabled = true
repo_path = "/ABS/PATH/TO/mervyn-worklog"   # same idea as MERVYN_WORKLOG_REPO
remote = "origin"
branch = "main"
pull_cron = "0 */5 * * * *"
```

`repo_path` is passed to `git` as the working directory. After a successful pull, Mervyn runs a **vault → redb sync** so symlinked `worklog.md` updates show up immediately.

Env example: `MERVYN__WORKLOG_GIT__ENABLED=true` and `MERVYN__WORKLOG_GIT__REPO_PATH=/path/to/clone`.

If Mervyn is stopped, pulls do not run (by design). For a machine where Mervyn is rarely up, you can still use OS cron as a fallback.

---

## Behaviour summary

| Step | What happens |
|------|----------------|
| You `git commit` under a path matching `MERVYN_WORKLOG_PROJECT_PREFIXES` | Hook runs (unless the commit is **inside** `MERVYN_WORKLOG_REPO`). |
| Hook | Appends `- **reponame** \`hash\` subject` under today’s `## YYYY-MM-DD` in `worklog.md`. |
| Hook | `git commit` + `git push` in the worklog repo. |
| Mervyn `[worklog_git]` job | `git pull` on `pull_cron`, then vault sync → `redb` (and the usual vault watcher still applies). |

---

## Backfill (missed hook runs)

If commits were made without the global hook (new machine, wrong `MERVYN_WORKLOG_PROJECT_PREFIXES`, etc.), run **`backfill_worklog_from_git.py`** from this folder. It reads the same **`~/.config/mervyn-worklog/env`**, walks git repos under your prefixes (skipping **`.terraform/`**, **`node_modules/`**, the worklog repo itself), and appends hook-style lines for commits whose hash is not already in **`worklog.md`**.

- **Date heading:** each commit goes under **`## YYYY-MM-DD`** using git’s author date (`%as`, author-local calendar day).
- **Time suffix:** backfilled lines include **`_HH:MM_`** from the author timestamp (wall clock from git’s `%ai`).
- **`--since`:** default **`auto`** uses the **earliest `## YYYY-MM-DD`** already present in **`worklog.md`** so you do not import all of **`~/Code`** history. Use **`--since none`** for a full import, or **`--since 2025-01-01`** for a custom cutoff.

```bash
python3 contrib/mervyn-worklog-sync/backfill_worklog_from_git.py --dry-run
python3 contrib/mervyn-worklog-sync/backfill_worklog_from_git.py
```

Then **`git commit`** / **`git push`** the worklog repo as usual.

---

## Optional toggles

- **`MERVYN_WORKLOG_SKIP_PUSH=1`** — append and commit locally; you push when online.
- **`MERVYN_WORKLOG_UTC_DATE=1`** — date headings in UTC instead of local timezone.

---

## Requirements

- **`python3`** on machines where you commit (for `append-worklog-line.py`).
- **`git`** and network access for **`git push`** / **`git pull`**.
- Path prefixes must not contain spaces (shell limitation in the hook loop).
