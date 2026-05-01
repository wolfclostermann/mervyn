#!/usr/bin/env bash
# Deploy Mervyn to a GCE instance over gcloud (IAP SSH).
#
# Reads ciphertext .env.enc from the repo (same format as scripts/env-crypto.sh), decrypts
# locally to a temp file, uploads it with the app bundle, then runs compose on the VM.
#
# With --local-build, the OCI image is built on this machine (podman preferred, else docker),
# saved, copied to the instance, and loaded; the VM only runs compose up, which
# skips a remote Rust build and is usually much faster. Default image platform is linux/arm64
# (assumed remote); for an amd64 host set MERVYN_DOCKER_PLATFORM=linux/amd64.
#
# Runtime (GCE): prefers Podman (podman-compose, then `podman compose`); if none, Docker. We install
# `podman-compose` via `pip install --user` (Jammy has no podman-compose deb) and prefer
# that over `podman compose` so a non-interactive shell does not depend on Podman 4's
# built-in compose subcommand (Podman 3 reports "unrecognized command compose").
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
# Optional env: MERVYN_GCP_PROJECT, MERVYN_GCP_ZONE (else gcloud config defaults apply),
#               MERVYN_DOCKER_PLATFORM (default: linux/arm64 for --local-build; use linux/amd64 for x86 VMs).
# Optional flags: --project=ID --zone=ZONE --dry-run --use-local-env (use ./.env instead of .env.enc)
#                 --local-build  build the image here (podman, else docker), then copy and load on the VM
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
LOCAL_BUILD=false
POSITIONAL=()

# Must match docker-compose.yml service mervyn image tag.
readonly DEPLOY_IMAGE="mervyn:deploy"
# Default: assume remote (GCE) is arm64; set MERVYN_DOCKER_PLATFORM=linux/amd64 for amd64.
readonly MERVYN_DOCKER_PLATFORM_DEFAULT="linux/arm64"

usage() {
  cat <<'EOF' >&2
Usage: deploy-gcp.sh [options] [INSTANCE]

  INSTANCE           GCE instance name (default: mervyn, or MERVYN_GCP_INSTANCE)

Options:
  --project=ID       gcloud --project (or MERVYN_GCP_PROJECT)
  --zone=ZONE        gcloud --zone (or MERVYN_GCP_ZONE)
  --dry-run          print commands only (no decrypt, upload, or remote build)
  --use-local-env    upload ./.env instead of decrypting .env.enc (do not commit .env)
  --local-build      build mervyn:deploy with podman (or docker) here, copy to VM, load, compose up (no --build on VM;
                     default target is linux/arm64; set MERVYN_DOCKER_PLATFORM=linux/amd64 for an x86 VM)
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
    --local-build) LOCAL_BUILD=true ;;
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
  if [[ "$LOCAL_BUILD" == true ]]; then
    echo "  podman (else docker) build --platform \${MERVYN_DOCKER_PLATFORM:-$MERVYN_DOCKER_PLATFORM_DEFAULT} -t $DEPLOY_IMAGE -f ... && (podman|docker) save ... $DEPLOY_IMAGE -o mervyn-image.tar"
    echo "  gcloud compute scp ... mervyn-image.tar ${INSTANCE}:~/mervyn-image.tar"
    echo "  gcloud compute ssh $INSTANCE --tunnel-through-iap ... --command='... load image ... up -d (no --build)'"
  else
    echo "  gcloud compute ssh $INSTANCE --tunnel-through-iap ... --command='ensure compose if needed ... up -d --build'"
  fi
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

IMAGE_TAR=""
if [[ "$LOCAL_BUILD" == true ]]; then
  LOCAL_OCI=""
  if command -v podman >/dev/null 2>&1; then
    LOCAL_OCI="podman"
  elif command -v docker >/dev/null 2>&1; then
    LOCAL_OCI="docker"
  else
    echo "error: --local-build requires podman or docker in PATH" >&2
    exit 1
  fi
  DOCKER_PLATFORM="${MERVYN_DOCKER_PLATFORM:-$MERVYN_DOCKER_PLATFORM_DEFAULT}"
  IMAGE_TAR="$STAGE/mervyn-image.tar"
  echo "Local build: $LOCAL_OCI build -t $DEPLOY_IMAGE (platform: $DOCKER_PLATFORM) ..."
  "$LOCAL_OCI" build --platform "$DOCKER_PLATFORM" -t "$DEPLOY_IMAGE" -f "$ROOT/Dockerfile" "$ROOT"
  echo "Local build: $LOCAL_OCI save -> mervyn-image.tar"
  "$LOCAL_OCI" save -o "$IMAGE_TAR" "$DEPLOY_IMAGE"
fi

gcloud_ssh --command='mkdir -p ~/mervyn-worklog ~/mervyn/data/vault ~/mervyn'

gcloud_scp "$ARCHIVE" "${INSTANCE}:~/mervyn-deploy.tgz"
if [[ -n "$IMAGE_TAR" ]]; then
  gcloud_scp "$IMAGE_TAR" "${INSTANCE}:~/mervyn-image.tar"
fi
gcloud_scp "$ENV_LOCAL" "${INSTANCE}:~/mervyn/.env"

REMOTE_CMD="$(cat "$ROOT/scripts/deploy-remote-setup.sh")"

GCE_CMD_PREFIX=""
[[ "$LOCAL_BUILD" == true ]] && GCE_CMD_PREFIX="export MERVYN_DEPLOY_MODE=local; "
gcloud_ssh --command="${GCE_CMD_PREFIX}${REMOTE_CMD}"

SSH_HINT="gcloud compute ssh ${INSTANCE} --tunnel-through-iap"
[[ -n "$PROJECT" ]] && SSH_HINT+=" --project=${PROJECT}"
[[ -n "$ZONE" ]] && SSH_HINT+=" --zone=${ZONE}"
echo "Deployed to instance ${INSTANCE}. SSH: ${SSH_HINT}"
