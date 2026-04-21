#!/usr/bin/env bash
# Deploy Mervyn to a GCE instance over gcloud (IAP SSH).
#
# Reads ciphertext .env.enc from the repo (same format as scripts/env-crypto.sh), decrypts
# locally to a temp file, uploads it with the app bundle, then runs compose on the VM.
#
# Runtime: prefers Docker (`docker compose`); otherwise Podman (`podman compose` or
# `podman-compose`). If no working compose stack exists, installs `podman` + `python3-pip`
# via apt and `podman-compose` via `pip install --user` (Jammy has no podman-compose deb).
# On Ubuntu 22.04/24.04, stock Podman 3.x / old Buildah can panic on multi-stage `podman build`;
# the script upgrades/installs Podman from Kubic (4.x+) when Docker Compose is not available.
# Rust builds run inside the image (Dockerfile); the host only needs a container runtime + compose.
# Podman runs rootful (`sudo`) for non-root SSH users so image builds/pulls do not hit rootless
# subuid/subgid mapping errors (e.g. lchown on /etc/gshadow in layers). Upgrading Ubuntu alone
# does not fix that; either widen /etc/subuid+subgid or use rootful Podman / Docker.
# Compose expects ~/mervyn and ~/mervyn-worklog (sibling of app dir) for the worklog bind mount.
#
# Usage:
#   ./scripts/deploy-gcp.sh [INSTANCE_NAME]
#   MERVYN_ENV_PASSPHRASE=... ./scripts/deploy-gcp.sh   # non-interactive decrypt
#
# Optional env: MERVYN_GCP_PROJECT, MERVYN_GCP_ZONE (else gcloud config defaults apply).
# Optional flags: --project=ID --zone=ZONE --dry-run --use-local-env (use ./.env instead of .env.enc)
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Keep in sync with scripts/env-crypto.sh
OPENSSL_CIPHER="-aes-256-cbc"
OPENSSL_PBKDF2_ITER=600000

INSTANCE="${MERVYN_GCP_INSTANCE:-mervyn}"
PROJECT="${MERVYN_GCP_PROJECT:-}"
ZONE="${MERVYN_GCP_ZONE:-}"
DRY_RUN=false
USE_LOCAL_ENV=false
POSITIONAL=()

usage() {
  cat <<'EOF' >&2
Usage: deploy-gcp.sh [options] [INSTANCE]

  INSTANCE           GCE instance name (default: mervyn, or MERVYN_GCP_INSTANCE)

Options:
  --project=ID       gcloud --project (or MERVYN_GCP_PROJECT)
  --zone=ZONE        gcloud --zone (or MERVYN_GCP_ZONE)
  --dry-run          print commands only (no decrypt, upload, or remote build)
  --use-local-env    upload ./.env instead of decrypting .env.enc (do not commit .env)
  -h, --help         this help

Decrypt:
  Interactive passphrase prompt, or set MERVYN_ENV_PASSPHRASE for non-interactive use.

Example:
  gcloud config set project my-project
  gcloud config set compute/zone europe-west2-a
  ./scripts/deploy-gcp.sh
  gcloud compute ssh mervyn --tunnel-through-iap
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project=*) PROJECT="${1#*=}" ;;
    --zone=*) ZONE="${1#*=}" ;;
    --dry-run) DRY_RUN=true ;;
    --use-local-env) USE_LOCAL_ENV=true ;;
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

gcloud_ssh() {
  local -a cmd=(gcloud compute ssh "$INSTANCE" --tunnel-through-iap)
  [[ -n "$PROJECT" ]] && cmd+=(--project="$PROJECT")
  [[ -n "$ZONE" ]] && cmd+=(--zone="$ZONE")
  "${cmd[@]}" "$@"
}

gcloud_scp() {
  local -a cmd=(gcloud compute scp --tunnel-through-iap)
  [[ -n "$PROJECT" ]] && cmd+=(--project="$PROJECT")
  [[ -n "$ZONE" ]] && cmd+=(--zone="$ZONE")
  "${cmd[@]}" "$@"
}

if [[ "$DRY_RUN" == true ]]; then
  echo "dry-run: instance=$INSTANCE project=${PROJECT:-<gcloud default>} zone=${ZONE:-<gcloud default>}"
  echo "dry-run: would decrypt .env.enc (or use --use-local-env), tar sources, then:"
  echo "  gcloud compute ssh $INSTANCE --tunnel-through-iap ... --command='ensure compose if needed ... up -d --build'"
  echo "  gcloud compute scp ... mervyn-deploy.tgz ${INSTANCE}:~/mervyn-deploy.tgz"
  echo "  gcloud compute scp ... .env ${INSTANCE}:~/mervyn/.env"
  exit 0
fi

TMPDIR="${TMPDIR:-/tmp}"
STAGE="$(mktemp -d "${TMPDIR%/}/mervyn-deploy.XXXXXX")"
cleanup() {
  rm -rf "$STAGE"
}
trap cleanup EXIT

ENV_LOCAL="$STAGE/.env"
ARCHIVE="$STAGE/mervyn-deploy.tgz"

if [[ "$USE_LOCAL_ENV" == true ]]; then
  if [[ ! -f "$ROOT/.env" ]]; then
    echo "error: --use-local-env but $ROOT/.env not found" >&2
    exit 1
  fi
  cp -a "$ROOT/.env" "$ENV_LOCAL"
else
  if [[ ! -f "$ROOT/.env.enc" ]]; then
    echo "error: $ROOT/.env.enc not found (commit ciphertext or use --use-local-env with ./.env)" >&2
    exit 1
  fi
  if [[ -n "${MERVYN_ENV_PASSPHRASE:-}" ]]; then
    printf '%s' "$MERVYN_ENV_PASSPHRASE" | openssl enc -d "$OPENSSL_CIPHER" -pbkdf2 -iter "$OPENSSL_PBKDF2_ITER" \
      -in "$ROOT/.env.enc" -out "$ENV_LOCAL" -pass stdin
  else
    read -rsp "Passphrase for .env.enc: " pass
    echo
    printf '%s' "$pass" | openssl enc -d "$OPENSSL_CIPHER" -pbkdf2 -iter "$OPENSSL_PBKDF2_ITER" \
      -in "$ROOT/.env.enc" -out "$ENV_LOCAL" -pass stdin
  fi
  chmod 600 "$ENV_LOCAL" 2>/dev/null || true
fi

# macOS tar adds xattr keys GNU tar on Linux warns about; suppress at source.
if [[ "$(uname -s)" == "Darwin" ]]; then
  export COPYFILE_DISABLE=1
fi

tar czf "$ARCHIVE" \
  -C "$ROOT" \
  --exclude='.git' \
  --exclude='target' \
  --exclude='data' \
  Dockerfile \
  docker-compose.yml \
  Cargo.toml \
  Cargo.lock \
  src \
  config

gcloud_ssh --command='mkdir -p ~/mervyn-worklog ~/mervyn/data/vault ~/mervyn'

gcloud_scp "$ARCHIVE" "${INSTANCE}:~/mervyn-deploy.tgz"
gcloud_scp "$ENV_LOCAL" "${INSTANCE}:~/mervyn/.env"

REMOTE_CMD=$(cat <<'REMOTE_SCRIPT'
set -euo pipefail
export PATH="${HOME}/.local/bin:${PATH}"

# A `docker` binary may exist without Compose (e.g. podman-docker shim) and break `docker compose up`.
has_working_compose() {
  if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    return 0
  fi
  if command -v docker-compose >/dev/null 2>&1; then
    return 0
  fi
  if command -v podman >/dev/null 2>&1; then
    if podman compose version >/dev/null 2>&1 || command -v podman-compose >/dev/null 2>&1; then
      return 0
    fi
  fi
  return 1
}

# Podman 3.x (Ubuntu universe) can hit buildah panics on multi-stage Dockerfiles; Podman 4+ fixes that.
ensure_podman_recent_for_multistage_build() {
  if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    return 0
  fi
  command -v apt-get >/dev/null 2>&1 || return 0
  [[ -r /etc/os-release ]] || return 0
  # shellcheck disable=SC1091
  . /etc/os-release
  [[ "${ID:-}" == "ubuntu" ]] || return 0
  # (Avoid `case … pattern)` here: `)` would close the outer `$(` that wraps this heredoc.)
  if [[ "${VERSION_ID:-}" != "22.04" && "${VERSION_ID:-}" != "24.04" ]]; then
    return 0
  fi

  local major=0
  if command -v podman >/dev/null 2>&1; then
    major=$(podman --version 2>/dev/null | awk '{print $3}' | cut -d. -f1)
    major=${major:-0}
  fi
  [[ "${major:-0}" -lt 4 ]] || return 0

  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl gnupg
  sudo mkdir -p /etc/apt/keyrings
  sudo rm -f /etc/apt/keyrings/devel_kubic_libcontainers_stable.gpg
  curl -fsSL "https://download.opensuse.org/repositories/devel:/kubic:/libcontainers:/stable/xUbuntu_${VERSION_ID}/Release.key" | sudo gpg --batch --no-tty --dearmor -o /etc/apt/keyrings/devel_kubic_libcontainers_stable.gpg
  echo "deb [signed-by=/etc/apt/keyrings/devel_kubic_libcontainers_stable.gpg] https://download.opensuse.org/repositories/devel:/kubic:/libcontainers:/stable/xUbuntu_${VERSION_ID}/ /" | sudo tee /etc/apt/sources.list.d/devel:kubic:libcontainers:stable.list >/dev/null
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y podman
}

ensure_container_runtime() {
  if ! command -v apt-get >/dev/null 2>&1; then
    echo "error: no working compose command found, and apt-get is missing; install docker compose or podman-compose manually." >&2
    exit 1
  fi
  ensure_podman_recent_for_multistage_build
  if has_working_compose; then
    return 0
  fi
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y python3-pip
  python3 -m pip install --user podman-compose
}

# Rootless Podman often fails unpacking layers that need wide UID/GID maps; rootful avoids that on a VPS.
podman_rootful() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  else
    sudo env "PATH=${PATH}" "HOME=${HOME}" "$@"
  fi
}

compose_cmd() {
  if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    docker compose "$@"
    return
  fi
  if command -v docker-compose >/dev/null 2>&1; then
    docker-compose "$@"
    return
  fi
  if command -v podman >/dev/null 2>&1; then
    if podman_rootful podman compose version >/dev/null 2>&1; then
      podman_rootful podman compose "$@"
    elif command -v podman-compose >/dev/null 2>&1; then
      podman_rootful podman-compose "$@"
    else
      echo "error: podman is installed but podman compose is not available (e.g. python3 -m pip install --user podman-compose)." >&2
      exit 1
    fi
    return
  fi
  echo "error: no compose command found after ensure_container_runtime" >&2
  exit 1
}

ensure_container_runtime
cd "${HOME}/mervyn"
tar xzf "${HOME}/mervyn-deploy.tgz"
rm -f "${HOME}/mervyn-deploy.tgz"
compose_cmd up -d --build
compose_cmd ps
REMOTE_SCRIPT
)

gcloud_ssh --command="$REMOTE_CMD"

SSH_HINT="gcloud compute ssh ${INSTANCE} --tunnel-through-iap"
[[ -n "$PROJECT" ]] && SSH_HINT+=" --project=${PROJECT}"
[[ -n "$ZONE" ]] && SSH_HINT+=" --zone=${ZONE}"
echo "Deployed to instance ${INSTANCE}. SSH: ${SSH_HINT}"
