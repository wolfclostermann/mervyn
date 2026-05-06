#!/usr/bin/env bash
# Helpers for deploy scripts: read terraform.tfvars scalars (simple key = value lines),
# resolve OCI SSH targets from Terraform state + tfvars.
#
# Override tfvars path: MERVYN_TFVARS=/path/to/terraform.tfvars

mervyn_deploy_tfvars_path() {
  local root=${1:?repo root}
  echo "${MERVYN_TFVARS:-$root/terraform/terraform.tfvars}"
}

# Print scalar value for key (quoted string, number, boolean, or null -> empty line).
mervyn_tfvars_get_scalar() {
  local key=$1 tfvars=$2
  [[ -f "$tfvars" ]] && [[ -n "$key" ]] || return 1
  awk -v k="$key" '
    BEGIN { pat = "^[[:space:]]*" k "[[:space:]]*=" }
    /^[[:space:]]*#/ { next }
    $0 ~ pat {
      line = $0
      sub(pat "[[:space:]]*", "", line)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
      if (line == "null") { print ""; exit 0 }
      if (line ~ /^".*"$/) { gsub(/^"|"$/, "", line); print line; exit 0 }
      if (line ~ /^true$|^false$/) { print line; exit 0 }
      if (line ~ /^[[:digit:]]+(\.[[:digit:]]+)?$/) { print line; exit 0 }
      print line
      exit 0
    }
  ' "$tfvars"
}

# ssh_user from tfvars, or from instance_image_os when ssh_user is empty/null.
mervyn_tfvars_ssh_user_effective() {
  local root=${1:?repo root}
  local tfvars u os
  tfvars=$(mervyn_deploy_tfvars_path "$root")
  [[ -f "$tfvars" ]] || return 1
  u=$(mervyn_tfvars_get_scalar ssh_user "$tfvars" || true)
  os=$(mervyn_tfvars_get_scalar instance_image_os "$tfvars" || true)
  if [[ -n "$u" ]]; then
    printf '%s' "$u"
    return 0
  fi
  case "${os:-oracle-linux}" in
    ubuntu) printf '%s' "ubuntu" ;;
    *) printf '%s' "opc" ;;
  esac
}

# terraform output instance_public_ip + ssh user from tfvars -> user@ip
mervyn_oci_ssh_from_tfvars_and_terraform() {
  local root=${1:?repo root}
  local tfvars ip user
  tfvars=$(mervyn_deploy_tfvars_path "$root")
  [[ -f "$tfvars" ]] || return 1
  ip=$(terraform -chdir="$root/terraform" output -raw instance_public_ip 2>/dev/null) || return 1
  [[ -n "$ip" && "$ip" != "null" ]] || return 1
  user=$(mervyn_tfvars_ssh_user_effective "$root") || return 1
  [[ -n "$user" ]] || return 1
  printf '%s@%s' "$user" "$ip"
}

# linux/arm64 if shape looks Ampere/ARM; else linux/amd64.
mervyn_docker_platform_hint_from_tfvars() {
  local root=${1:?repo root}
  local tfvars shape
  tfvars=$(mervyn_deploy_tfvars_path "$root")
  [[ -f "$tfvars" ]] || return 1
  shape=$(mervyn_tfvars_get_scalar instance_shape "$tfvars" || true)
  shape=${shape:-}
  case "$shape" in
    *A1*|*a1*|*ARM*|*Ampere*|*ampere*) printf '%s' "linux/arm64" ;;
    *) printf '%s' "linux/amd64" ;;
  esac
}

# Write shell-env file for deploy-remote-setup.sh (non-secret hints only).
mervyn_write_deploy_remote_env_from_tfvars() {
  local root=${1:?repo root}
  local out=${2:?output path}
  local tfvars os shape ver
  tfvars=$(mervyn_deploy_tfvars_path "$root")
  : >"$out"
  [[ -f "$tfvars" ]] || return 0
  os=$(mervyn_tfvars_get_scalar instance_image_os "$tfvars" || true)
  shape=$(mervyn_tfvars_get_scalar instance_shape "$tfvars" || true)
  ver=$(mervyn_tfvars_get_scalar oracle_linux_version "$tfvars" || true)
  [[ -n "$os" ]] && printf 'MERVYN_TFVARS_INSTANCE_IMAGE_OS=%q\n' "$os" >>"$out"
  [[ -n "$shape" ]] && printf 'MERVYN_TFVARS_INSTANCE_SHAPE=%q\n' "$shape" >>"$out"
  [[ -n "$ver" ]] && printf 'MERVYN_TFVARS_ORACLE_LINUX_VERSION=%q\n' "$ver" >>"$out"
}
