#!/usr/bin/env bash
# Mervyn GCP stack — same driver as terraform-framework-example-stack (terraform-framework Git
# modules, terraform-core remote state, shared-config backend).
#
# Usage: ./run.sh <dev|staging|prod> [plan|apply|destroy] [--local] [--yes|-auto-approve]
#
# Auth: Application Default Credentials + optional elevated SA impersonation (see TF_VAR_elevated_impersonate_service_account).
# VM: gcp-compute-vm os_login (framework) — OS Login metadata + optional IAM/IAP; see terraform/variables.tf.
#
# shared-config: FRAMEWORK_ROOT, sibling ../terraform-framework, or sparse clone (see gfs-sre/terraform-framework-example-stack run.sh).
#
# Local dev (uncomment as needed; re-comment before release if you track defaults in git):
# export TERRAFORM_FRAMEWORK_GIT_REF=development
# export TERRAFORM_FRAMEWORK_GIT_URL="https://github.com/gfs-sre/terraform-framework.git"
# export TERRAFORM_FRAMEWORK_SKIP_GIT_UPDATE=1

set -e

readonly STACK_ID="mervyn-gcp"
readonly DEFAULT_ELEVATED_IMPERSONATE_SA="project-factory-6046@terraform-core-452622.iam.gserviceaccount.com"
readonly FRAMEWORK_GIT_REF_DEFAULT="main"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TERRAFORM_DIR="${SCRIPT_DIR}/terraform"

framework_git_ref() {
  echo "${TERRAFORM_FRAMEWORK_GIT_REF:-${FRAMEWORK_GIT_REF_DEFAULT}}"
}

sync_terraform_framework_repo() {
  local root=$1
  local ref
  ref=$(framework_git_ref)

  [[ -d "${root}/.git" ]] || return 0

  if [[ "${TERRAFORM_FRAMEWORK_SKIP_GIT_UPDATE:-0}" == "1" ]]; then
    return 0
  fi

  if [[ -n "$(git -C "$root" status --porcelain 2>/dev/null)" ]]; then
    echo "terraform-framework at ${root} has local changes." >&2
    echo "Stash or commit, or set TERRAFORM_FRAMEWORK_SKIP_GIT_UPDATE=1." >&2
    exit 1
  fi

  git -C "$root" fetch --prune origin "+refs/heads/${ref}:refs/remotes/origin/${ref}" 2>/dev/null \
    || git -C "$root" fetch --prune origin "refs/heads/${ref}:refs/remotes/origin/${ref}" 2>/dev/null \
    || git -C "$root" fetch --prune origin "$ref"

  if ! git -C "$root" show-ref --verify --quiet "refs/remotes/origin/${ref}"; then
    echo "Branch '${ref}' not found on origin for ${root}." >&2
    exit 1
  fi

  git -C "$root" checkout -q "$ref" 2>/dev/null || git -C "$root" checkout -q -B "$ref" "origin/${ref}"

  if ! git -C "$root" merge -q --ff-only "origin/${ref}" 2>/dev/null; then
    git -C "$root" reset -q --hard "origin/${ref}"
  fi

  if [[ "$(git -C "$root" config --bool core.sparseCheckout 2>/dev/null || true)" == "true" ]]; then
    git -C "$root" sparse-checkout set shared-config 2>/dev/null || true
    git -C "$root" sparse-checkout reapply 2>/dev/null || true
  fi
}

resolve_shared_config() {
  local ref
  ref=$(framework_git_ref)

  if [[ -n "${FRAMEWORK_ROOT:-}" && -d "${FRAMEWORK_ROOT}/shared-config/shared" ]]; then
    sync_terraform_framework_repo "${FRAMEWORK_ROOT}"
    SHARED_CONFIG="${FRAMEWORK_ROOT}/shared-config"
    return 0
  fi

  local parent
  parent="$(cd "${SCRIPT_DIR}/.." && pwd)"
  if [[ -d "${parent}/terraform-framework/shared-config/shared" ]]; then
    FRAMEWORK_ROOT="${parent}/terraform-framework"
    sync_terraform_framework_repo "${FRAMEWORK_ROOT}"
    SHARED_CONFIG="${FRAMEWORK_ROOT}/shared-config"
    return 0
  fi

  local url="${TERRAFORM_FRAMEWORK_GIT_URL:-https://github.com/gfs-sre/terraform-framework.git}"
  local cache="${TERRAFORM_FRAMEWORK_SHARED_CONFIG_CACHE:-${HOME}/.cache/gfs-terraform-framework/${ref}}"

  if [[ ! -d "${cache}/.git" ]]; then
    mkdir -p "$(dirname "$cache")"
    rm -rf "$cache"
    git clone --filter=blob:none --sparse --depth 1 --branch "$ref" "$url" "$cache"
    (cd "$cache" && git sparse-checkout set shared-config)
  else
    sync_terraform_framework_repo "$cache"
  fi

  if [[ ! -d "${cache}/shared-config/shared" ]]; then
    echo "Could not load shared-config from ${url} (ref ${ref})." >&2
    exit 1
  fi

  FRAMEWORK_ROOT="$cache"
  SHARED_CONFIG="${cache}/shared-config"
}

resolve_shared_config

ENV="${1:-dev}"
ACTION="${2:-plan}"

if [[ ! "$ENV" =~ ^(dev|staging|prod)$ ]]; then
  echo "Usage: $0 <dev|staging|prod> [plan|apply|destroy] [--local] [--yes|-auto-approve]" >&2
  exit 1
fi

if [[ ! "$ACTION" =~ ^(plan|apply|destroy)$ ]]; then
  echo "Usage: $0 <dev|staging|prod> [plan|apply|destroy] [--local] [--yes|-auto-approve]" >&2
  exit 1
fi

USE_LOCAL_BACKEND=false
AUTO_APPROVE_DESTROY=false
for arg in "${@:3}"; do
  case "$arg" in
    --local) USE_LOCAL_BACKEND=true ;;
    --yes | -auto-approve) AUTO_APPROVE_DESTROY=true ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 1
      ;;
  esac
done

if [[ "${DISABLE_ELEVATED_IMPERSONATE:-0}" == "1" ]]; then
  unset TF_VAR_elevated_impersonate_service_account 2>/dev/null || true
elif [[ -z "${TF_VAR_elevated_impersonate_service_account+set}" ]]; then
  export TF_VAR_elevated_impersonate_service_account="${ELEVATED_IMPERSONATE_SERVICE_ACCOUNT:-${DEFAULT_ELEVATED_IMPERSONATE_SA}}"
fi

cd "${TERRAFORM_DIR}"

if [[ "$USE_LOCAL_BACKEND" == true ]]; then
  rm -f backend.tf
  terraform init -reconfigure -upgrade
else
  cp -f backend.gcs.tf.example backend.tf
  terraform init -reconfigure -upgrade \
    -backend-config="prefix=stacks/${STACK_ID}/${ENV}" \
    -backend-config="${SHARED_CONFIG}/shared/backend.config.hcl"
fi

VAR_FILES=("-var-file=envs/${ENV}.tfvars")

if [[ "$ACTION" == "apply" ]]; then
  terraform apply -auto-approve "${VAR_FILES[@]}"
elif [[ "$ACTION" == "destroy" ]]; then
  if [[ "$AUTO_APPROVE_DESTROY" == true ]]; then
    terraform destroy -auto-approve "${VAR_FILES[@]}"
  else
    terraform destroy "${VAR_FILES[@]}"
  fi
else
  terraform plan "${VAR_FILES[@]}"
fi
