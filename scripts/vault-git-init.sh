#!/usr/bin/env bash
# Turn an existing vault directory into the working tree of a git repository, folding whatever is
# already there into the remote's history.
#
# Written for the move away from the old layout, where the vault held a `worklog.md` symlink into
# a separate clone. The worklog repo becomes the vault repo: its `worklog.md` arrives as an
# ordinary file, and the vault's own `events.md` / `todos.md` / `reminders.md` are committed
# alongside it.
#
# Idempotent: a vault that is already a clone of the right remote is left alone.
#
# Usage:
#   ./scripts/vault-git-init.sh <vault-dir> <remote-url> [branch]
#   MERVYN_VAULT_GITHUB_PAT=ghp_... ./scripts/vault-git-init.sh ~/mervyn/data/vault \
#       https://github.com/you/mervyn-vault.git main
#
# The token needs **write** access, and is used only for this run — it is never written to
# .git/config, so Mervyn supplies its own on every later call.

set -euo pipefail

VAULT="${1:-}"
REMOTE_URL="${2:-}"
BRANCH="${3:-main}"

if [[ -z "$VAULT" || -z "$REMOTE_URL" ]]; then
  echo "usage: $0 <vault-dir> <remote-url> [branch]" >&2
  exit 1
fi
[[ -d "$VAULT" ]] || { echo "error: $VAULT is not a directory" >&2; exit 1; }

VAULT="$(cd "$VAULT" && pwd)"

git_auth=()
if [[ -n "${MERVYN_VAULT_GITHUB_PAT:-}" ]]; then
  basic="$(printf 'x-access-token:%s' "$MERVYN_VAULT_GITHUB_PAT" | base64 | tr -d '\n')"
  git_auth=(-c "http.https://github.com/.extraheader=AUTHORIZATION: basic ${basic}")
fi
# ${a[@]+...} keeps `set -u` happy with an empty array on bash 3.2 (macOS).
g() { git ${git_auth[@]+"${git_auth[@]}"} -C "$VAULT" "$@"; }

if [[ -e "$VAULT/.git" ]]; then
  echo "[vault-git-init] $VAULT is already a repository; leaving it alone."
  g remote -v
  exit 0
fi

stamp="$(date +%Y%m%d-%H%M%S)"
backup="${VAULT%/}.backup.pre-git-${stamp}"
echo "[vault-git-init] backing up to $backup"
cp -RL "$VAULT" "$backup" 2>/dev/null || cp -R "$VAULT" "$backup"

# The old worklog.md is a symlink into the separate clone, and the remote carries the real file.
if [[ -L "$VAULT/worklog.md" ]]; then
  echo "[vault-git-init] removing the worklog.md symlink (the remote has the real file)"
  rm "$VAULT/worklog.md"
fi

echo "[vault-git-init] initialising and fetching $REMOTE_URL ($BRANCH)"
g init -q
g remote add origin "$REMOTE_URL"
g fetch -q origin "$BRANCH"
# -B resets the branch to the remote; untracked files (events.md, todos.md) are left in place.
g checkout -q -f -B "$BRANCH" "origin/$BRANCH"

if [[ -n "$(g status --porcelain)" ]]; then
  echo "[vault-git-init] committing what the vault already held"
  g add -A
  g -c user.name=Mervyn -c user.email=mervyn@localhost \
    commit -q -m "vault: fold Mervyn's vault files into the worklog repo"
  g push -q origin "$BRANCH"
  echo "[vault-git-init] pushed."
else
  echo "[vault-git-init] nothing local to add."
fi

echo "[vault-git-init] done. Contents:"
ls -la "$VAULT"
