#!/usr/bin/env bash
# Deploy Mervyn to an OCI (or any Ubuntu) VM over SSH — same bundle and remote logic as deploy-gcp.sh,
# but uses scp/ssh instead of gcloud IAP.
#
# Target host should match oci-bootstrap.sh / Terraform: Ubuntu user `ubuntu`, Docker or Podman from cloud-init.
#
# Reads ciphertext .env.enc from the repo (same format as scripts/env-crypto.sh), decrypts locally,
# uploads with the app bundle, then runs compose on the VM.
#
# With --local-build, build the image locally (podman preferred, else docker), save, copy, load on the VM.
# Default platform is linux/arm64 (Ampere); set MERVYN_DOCKER_PLATFORM=linux/amd64 for x86 VMs.
#
# Usage:
#   ./scripts/deploy-oci.sh ubuntu@203.0.113.7
#   MERVYN_OCI_SSH=ubuntu@203.0.113.7 ./scripts/deploy-oci.sh --local-build
#   MERVYN_ENV_PASSPHRASE=... ./scripts/deploy-oci.sh ubuntu@host
#
# Optional env: MERVYN_DOCKER_PLATFORM, MERVYN_SSH_OPTS (extra ssh/scp words, e.g. -o ProxyJump=jumphost)
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OPENSSL_CIPHER="-aes-256-cbc"
OPENSSL_PBKDF2_ITER=600000

SSH_DEST="${MERVYN_OCI_SSH:-}"
SSH_EXPLICIT=""
DRY_RUN=false
USE_LOCAL_ENV=false
LOCAL_BUILD=false
POSITIONAL=()

readonly DEPLOY_IMAGE="mervyn:deploy"
readonly MERVYN_DOCKER_PLATFORM_DEFAULT="linux/arm64"
readonly REMOTE_SETUP="$ROOT/scripts/deploy-remote-setup.sh"

usage() {
  cat <<'EOF' >&2
Usage: deploy-oci.sh [options] [user@host]

  user@host          SSH destination (or set MERVYN_OCI_SSH)

Options:
  --ssh=user@host    same as positional (wins over MERVYN_OCI_SSH if both given)
  --dry-run          print actions only
  --use-local-env    upload ./.env instead of decrypting .env.enc
  --local-build      build image here, copy tarball, load on VM (no remote --build)
  -h, --help         this help

Decrypt:
  Interactive passphrase, or MERVYN_ENV_PASSPHRASE for non-interactive use.

Examples:
  ./scripts/deploy-oci.sh ubuntu@$(terraform -chdir=terraform output -raw instance_public_ip)
  MERVYN_SSH_OPTS='-o StrictHostKeyChecking=accept-new' ./scripts/deploy-oci.sh ubuntu@203.0.113.7
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --ssh=*)
      SSH_EXPLICIT="${1#*=}"
      ;;
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
  SSH_DEST="${POSITIONAL[0]}"
fi
if [[ -n "$SSH_EXPLICIT" ]]; then
  SSH_DEST="$SSH_EXPLICIT"
fi

if [[ -z "$SSH_DEST" ]]; then
  echo "error: set MERVYN_OCI_SSH or pass user@host (see --help)" >&2
  exit 1
fi

if [[ ! -f "$REMOTE_SETUP" ]]; then
  echo "error: missing $REMOTE_SETUP" >&2
  exit 1
fi

SSH_BASE_OPTS=()
if [[ -n "${MERVYN_SSH_OPTS:-}" ]]; then
  # shellcheck disable=SC2206
  SSH_BASE_OPTS=( $MERVYN_SSH_OPTS )
fi

run_ssh() {
  ssh "${SSH_BASE_OPTS[@]}" "$SSH_DEST" "$@"
}

run_scp() {
  scp "${SSH_BASE_OPTS[@]}" "$@"
}

if [[ "$DRY_RUN" == true ]]; then
  echo "dry-run: ssh_dest=$SSH_DEST"
  echo "dry-run: would decrypt .env.enc (or --use-local-env), tar sources, then:"
  if [[ "$LOCAL_BUILD" == true ]]; then
    echo "  (local) podman|docker build ... && save -> mervyn-image.tar"
    echo "  scp ... mervyn-image.tar ${SSH_DEST}:~/mervyn-image.tar"
  fi
  echo "  scp ... mervyn-deploy.tgz ${SSH_DEST}:~/mervyn-deploy.tgz"
  echo "  scp ... .env ${SSH_DEST}:~/mervyn/.env"
  echo "  ssh ... bash -s < deploy-remote-setup.sh"
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
    echo "error: $ROOT/.env.enc not found (commit ciphertext or use --use-local-env)" >&2
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

run_ssh mkdir -p ~/mervyn-worklog ~/mervyn/data/vault ~/mervyn

run_scp "$ARCHIVE" "${SSH_DEST}:~/mervyn-deploy.tgz"
if [[ -n "$IMAGE_TAR" ]]; then
  run_scp "$IMAGE_TAR" "${SSH_DEST}:~/mervyn-image.tar"
fi
run_scp "$ENV_LOCAL" "${SSH_DEST}:~/mervyn/.env"

{
  if [[ "$LOCAL_BUILD" == true ]]; then
    echo 'export MERVYN_DEPLOY_MODE=local'
  fi
  cat "$REMOTE_SETUP"
} | run_ssh bash -s

if [[ ${#SSH_BASE_OPTS[@]} -gt 0 ]]; then
  echo "Deployed to ${SSH_DEST}. SSH: ssh ${SSH_BASE_OPTS[*]} $SSH_DEST"
else
  echo "Deployed to ${SSH_DEST}. SSH: ssh $SSH_DEST"
fi
