#!/usr/bin/env bash
# Copy local Mervyn data (redb + vault, optionally worklog repo) to the cloud host.
#
# Default transport matches scripts/deploy-gcp.sh: rsync over `gcloud compute ssh`
# (IAP tunnel). For Oracle / any host with normal SSH, use --ssh user@host.
#
# redb is a single file: avoid copying while either side is writing. This script
# can stop the remote compose stack before upload and bring it back with `compose up -d`
# after (--remote-stop / default on). `compose start` is not used: it fails when no
# container exists yet. Stop your local process (cargo run / docker compose) yourself before
# running, or use --local-stop to try `docker compose stop` in this repo.
#
# Usage (GCP, same env/flags as deploy-gcp.sh):
#   ./scripts/sync-data-to-cloud.sh
#   MERVYN_GCP_PROJECT=myproj MERVYN_GCP_ZONE=europe-west2-a ./scripts/sync-data-to-cloud.sh mervyn
#
# Plain SSH (rsync):
#   ./scripts/sync-data-to-cloud.sh --ssh ubuntu@203.0.113.7
#
# Optional:
#   --dry-run              print actions only
#   --no-remote-stop       do not stop remote compose before copy (unsafe if mervyn is running)
#   --no-remote-start      after copy, do not start the remote stack again
#   --local-stop           run `docker compose stop` from repo root (best effort)
#   --with-worklog         also sync ../mervyn-worklog → remote ~/mervyn-worklog
#   --worklog-path=PATH    override local worklog directory for --with-worklog
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

INSTANCE="${MERVYN_GCP_INSTANCE:-mervyn}"
PROJECT="${MERVYN_GCP_PROJECT:-}"
ZONE="${MERVYN_GCP_ZONE:-}"
DRY_RUN=false
REMOTE_STOP=true
REMOTE_START=true
LOCAL_STOP=false
WITH_WORKLOG=false
WORKLOG_PATH="${MERVYN_WORKLOG_PATH:-$ROOT/../mervyn-worklog}"
SSH_DEST=""
DATA_DIR="${MERVYN_LOCAL_DATA_DIR:-$ROOT/data}"
POSITIONAL=()

usage() {
  cat <<'EOF' >&2
Usage: sync-data-to-cloud.sh [options] [INSTANCE]

  INSTANCE           GCE instance name (default: mervyn, or MERVYN_GCP_INSTANCE)

Options:
  --project=ID         gcloud --project (or MERVYN_GCP_PROJECT)
  --zone=ZONE          gcloud --zone (or MERVYN_GCP_ZONE)
  --ssh=user@host      use rsync over ssh instead of gcloud (INSTANCE arg ignored)
  --dry-run
  --no-remote-stop     skip stopping remote compose before copy
  --no-remote-start    do not start remote compose after copy
  --local-stop         try docker compose stop in repo root before copy
  --with-worklog       sync MERVYN_WORKLOG_PATH or ../mervyn-worklog to ~/mervyn-worklog
  --worklog-path=PATH  local path for --with-worklog
  -h, --help

Examples:
  ./scripts/sync-data-to-cloud.sh
  ./scripts/sync-data-to-cloud.sh --ssh ubuntu@203.0.113.7 --with-worklog
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project=*) PROJECT="${1#*=}" ;;
    --zone=*) ZONE="${1#*=}" ;;
    --ssh) SSH_DEST="${2:-}"; shift ;;
    --ssh=*) SSH_DEST="${1#*=}" ;;
    --dry-run) DRY_RUN=true ;;
    --no-remote-stop) REMOTE_STOP=false ;;
    --no-remote-start) REMOTE_START=false ;;
    --local-stop) LOCAL_STOP=true ;;
    --with-worklog) WITH_WORKLOG=true ;;
    --worklog-path=*) WORKLOG_PATH="${1#*=}" ;;
    -h | --help) usage; exit 0 ;;
    -*)
      echo "error: unknown option: $1" >&2
      usage
      exit 1
      ;;
    *)
      POSITIONAL+=("$1")
      ;;
  esac
  shift
done

if [[ ${#POSITIONAL[@]} -gt 1 ]]; then
  echo "error: too many arguments: ${POSITIONAL[*]}" >&2
  exit 1
fi
if [[ ${#POSITIONAL[@]} -eq 1 ]]; then
  INSTANCE="${POSITIONAL[0]}"
fi

if [[ ! -d "$DATA_DIR" ]]; then
  echo "error: local data directory not found: $DATA_DIR" >&2
  exit 1
fi

gcloud_ssh() {
  local -a cmd=(gcloud compute ssh "$INSTANCE" --tunnel-through-iap)
  [[ -n "$PROJECT" ]] && cmd+=(--project="$PROJECT")
  [[ -n "$ZONE" ]] && cmd+=(--zone="$ZONE")
  "${cmd[@]}" "$@"
}

# Remote: stop/start compose in ~/mervyn (same layout as deploy-gcp.sh).
# - stop: only if containers exist (`compose start`/`stop` error when none, e.g. first boot).
# - start: `up -d` creates/recreates the service; `start` only wakes existing stopped containers.
remote_compose() {
  local remote_action="$1"
  cat <<REMOTE
set -euo pipefail
export PATH="\${HOME}/.local/bin:\${PATH}"
cd "\${HOME}/mervyn" || exit 0
maybe_fix_podman_cni_conflists() {
  command -v podman >/dev/null 2>&1 || return 0
  local dir f
  for dir in /etc/cni/net.d; do
    [[ -d "\$dir" ]] || continue
    for f in "\$dir"/*.conflist; do
      [[ -f "\$f" ]] || continue
      if grep -qE '"cniVersion"[[:space:]]*:[[:space:]]*"1\\.0\\.0"' "\$f" 2>/dev/null; then
        if [[ "\$(id -u)" -eq 0 ]]; then
          sed -i 's/"cniVersion"[[:space:]]*:[[:space:]]*"1\\.0\\.0"/"cniVersion": "0.4.0"/g' "\$f"
        else
          sudo sed -i 's/"cniVersion"[[:space:]]*:[[:space:]]*"1\\.0\\.0"/"cniVersion": "0.4.0"/g' "\$f"
        fi
      fi
    done
  done
}
[[ "$remote_action" != stop ]] && maybe_fix_podman_cni_conflists
if docker compose version >/dev/null 2>&1; then
  if [[ "$remote_action" == stop ]]; then
    if docker compose ps -aq 2>/dev/null | grep -q .; then
      docker compose stop || true
    fi
  else
    docker compose up -d
  fi
elif command -v docker-compose >/dev/null 2>&1; then
  if [[ "$remote_action" == stop ]]; then
    if docker-compose ps -aq 2>/dev/null | grep -q .; then
      docker-compose stop || true
    fi
  else
    docker-compose up -d
  fi
elif command -v podman-compose >/dev/null 2>&1; then
  if [[ "$remote_action" == stop ]]; then
    if [[ "\$(id -u)" -eq 0 ]]; then
      if podman-compose ps -aq 2>/dev/null | grep -q .; then
        podman-compose stop || true
      fi
    else
      if sudo env "PATH=\${PATH}" "HOME=\${HOME}" podman-compose ps -aq 2>/dev/null | grep -q .; then
        sudo env "PATH=\${PATH}" "HOME=\${HOME}" podman-compose stop || true
      fi
    fi
  else
    if [[ "\$(id -u)" -eq 0 ]]; then
      podman-compose up -d
    else
      sudo env "PATH=\${PATH}" "HOME=\${HOME}" podman-compose up -d
    fi
  fi
elif podman compose version >/dev/null 2>&1; then
  if [[ "$remote_action" == stop ]]; then
    if [[ "\$(id -u)" -eq 0 ]]; then
      if podman compose ps -aq 2>/dev/null | grep -q .; then
        podman compose stop || true
      fi
    else
      if sudo env "PATH=\${PATH}" "HOME=\${HOME}" podman compose ps -aq 2>/dev/null | grep -q .; then
        sudo env "PATH=\${PATH}" "HOME=\${HOME}" podman compose stop || true
      fi
    fi
  else
    if [[ "\$(id -u)" -eq 0 ]]; then
      podman compose up -d
    else
      sudo env "PATH=\${PATH}" "HOME=\${HOME}" podman compose up -d
    fi
  fi
else
  echo "warning: no compose on remote; ensure Mervyn is stopped before replacing data." >&2
fi
REMOTE
}

run() {
  if [[ "$DRY_RUN" == true ]]; then
    echo "dry-run:" "$@" >&2
    return 0
  fi
  "$@"
}

if [[ "$LOCAL_STOP" == true ]]; then
  if [[ -f "$ROOT/docker-compose.yml" ]] && command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    run sh -c "cd \"$ROOT\" && docker compose stop" || true
  else
    echo "warning: --local-stop requested but docker compose not available or no docker-compose.yml" >&2
  fi
fi

echo "Copying local data from: $DATA_DIR"
echo "Stop local Mervyn (cargo / docker) before upload if it is running, to avoid a half-written redb."

if [[ -n "$SSH_DEST" ]]; then
  if ! command -v rsync >/dev/null 2>&1; then
    echo "error: rsync is required for --ssh" >&2
    exit 1
  fi
  RSYNC=(rsync -av --delete-after)
  [[ "$DRY_RUN" == true ]] && RSYNC+=(--dry-run)
  if [[ "$REMOTE_STOP" == true ]]; then
    echo "Stopping remote compose via SSH..."
    run ssh "$SSH_DEST" "bash -s" <<<"$(remote_compose stop)" || true
  fi
  run ssh "$SSH_DEST" "mkdir -p ~/mervyn/data/vault ~/mervyn-worklog"
  echo "Rsync data/ → ${SSH_DEST}:~/mervyn/data/"
  run "${RSYNC[@]}" -e ssh "$DATA_DIR/" "${SSH_DEST}:~/mervyn/data/"
  if [[ "$WITH_WORKLOG" == true ]]; then
    if [[ ! -d "$WORKLOG_PATH" ]]; then
      echo "error: worklog path not found: $WORKLOG_PATH" >&2
      exit 1
    fi
    echo "Rsync worklog → ${SSH_DEST}:~/mervyn-worklog/"
    run "${RSYNC[@]}" -e ssh "$WORKLOG_PATH/" "${SSH_DEST}:~/mervyn-worklog/"
  fi
  if [[ "$REMOTE_START" == true ]]; then
    echo "Starting remote compose..."
    run ssh "$SSH_DEST" "bash -s" <<<"$(remote_compose start)" || true
  fi
else
  if ! command -v gcloud >/dev/null 2>&1; then
    echo "error: gcloud not in PATH (install Google Cloud SDK or use --ssh user@host)" >&2
    exit 1
  fi
  if ! command -v rsync >/dev/null 2>&1; then
    echo "error: rsync is required (install rsync or use a package manager)" >&2
    exit 1
  fi
  if [[ "$(uname -s)" == "Darwin" ]]; then
    export COPYFILE_DISABLE=1
  fi
  # rsync runs: <rsh> INSTANCE rsync --server …
  # gcloud needs: compute ssh INSTANCE …flags… -- rsync --server …
  # so we use a one-line wrapper (INSTANCE is $1, remote rsync is "$@").
  GSSH_WRAPPER="$(mktemp "${TMPDIR:-/tmp}/mervyn-gcloud-rsh.XXXXXX")"
  chmod +x "$GSSH_WRAPPER"
  _cleanup_gssh_wrapper() { rm -f "${GSSH_WRAPPER:-}"; }
  trap _cleanup_gssh_wrapper EXIT
  {
    echo '#!/bin/sh'
    echo 'set -e'
    echo 'h="$1"'
    echo 'shift'
    printf '%s\n' 'exec gcloud compute ssh "$h" --tunnel-through-iap \'
    [[ -n "$PROJECT" ]] && printf '%s\n' "  --project=${PROJECT} \\"
    [[ -n "$ZONE" ]] && printf '%s\n' "  --zone=${ZONE} \\"
    printf '%s\n' '  -- "$@"'
  } >"$GSSH_WRAPPER"
  RSYNC_G=(rsync -av --delete-after)
  [[ "$DRY_RUN" == true ]] && RSYNC_G+=(--dry-run)
  if [[ "$REMOTE_STOP" == true ]]; then
    echo "Stopping remote compose on ${INSTANCE}..."
    run gcloud_ssh --command="$(remote_compose stop)" || true
  fi
  run gcloud_ssh --command='mkdir -p ~/mervyn/data/vault ~/mervyn-worklog'
  echo "Rsync data/ → ${INSTANCE}:~/mervyn/data/ ..."
  run "${RSYNC_G[@]}" -e "$GSSH_WRAPPER" "$DATA_DIR/" "${INSTANCE}:~/mervyn/data/"
  if [[ "$WITH_WORKLOG" == true ]]; then
    if [[ ! -d "$WORKLOG_PATH" ]]; then
      echo "error: worklog path not found: $WORKLOG_PATH" >&2
      exit 1
    fi
    echo "Rsync worklog → ${INSTANCE}:~/mervyn-worklog/ ..."
    run "${RSYNC_G[@]}" -e "$GSSH_WRAPPER" "$WORKLOG_PATH/" "${INSTANCE}:~/mervyn-worklog/"
  fi
  if [[ "$REMOTE_START" == true ]]; then
    echo "Starting remote compose on ${INSTANCE}..."
    run gcloud_ssh --command="$(remote_compose start)" || true
  fi
fi

echo "Done."
