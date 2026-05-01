#!/usr/bin/env bash
# Run on the deployment VM only (invoked via SSH by deploy-gcp.sh / deploy-oci.sh).
set -euo pipefail
export PATH="${HOME}/.local/bin:${PATH}"

# A `docker` binary may exist without Compose (e.g. podman-docker shim) and break `docker compose up`.
can_use_podman_compose() {
  command -v podman >/dev/null 2>&1 || return 1
  if command -v podman-compose >/dev/null 2>&1; then
    return 0
  fi
  if podman compose version >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

has_working_compose() {
  if can_use_podman_compose; then
    return 0
  fi
  if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    return 0
  fi
  if command -v docker-compose >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

# Podman 3.x (Ubuntu universe) can hit buildah panics on multi-stage Dockerfiles; Podman 4+ fixes that.
ensure_podman_recent_for_multistage_build() {
  if can_use_podman_compose; then
    :
  else
    if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
      return 0
    fi
    if command -v docker-compose >/dev/null 2>&1; then
      return 0
    fi
  fi
  command -v apt-get >/dev/null 2>&1 || return 0
  [[ -r /etc/os-release ]] || return 0
  # shellcheck disable=SC1091
  . /etc/os-release
  [[ "${ID:-}" == "ubuntu" ]] || return 0
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
  # Pre-built image: no remote multi-stage build; stock Podman 3 can still load and run the image.
  if [[ "${MERVYN_DEPLOY_MODE:-}" != "local" ]]; then
    ensure_podman_recent_for_multistage_build
  fi
  if has_working_compose; then
    return 0
  fi
  sudo apt-get update -qq
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y python3-pip
  python3 -m pip install --user podman-compose
  hash -r 2>/dev/null || true
  export PATH="${HOME}/.local/bin:${PATH}"
  if ! command -v podman-compose >/dev/null 2>&1; then
    echo "error: pip install --user podman-compose did not put podman-compose on PATH (~/.local/bin missing from PATH?)." >&2
    exit 1
  fi
}

# Rootless Podman often fails unpacking layers that need wide UID/GID maps; rootful avoids that on a VPS.
podman_rootful() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  else
    sudo env "PATH=${PATH}" "HOME=${HOME}" "$@"
  fi
}

# Podman-compose on Ubuntu Jammy/Noble often writes /etc/cni/net.d/*_default.conflist with
# "cniVersion":"1.0.0" while distro CNI plugins only accept 0.4.x — noisy validation warnings.
maybe_fix_podman_cni_conflists() {
  command -v podman >/dev/null 2>&1 || return 0
  local dir f
  for dir in /etc/cni/net.d; do
    [[ -d "$dir" ]] || continue
    for f in "$dir"/*.conflist; do
      [[ -f "$f" ]] || continue
      if grep -qE '"cniVersion"[[:space:]]*:[[:space:]]*"1\.0\.0"' "$f" 2>/dev/null; then
        if [[ "$(id -u)" -eq 0 ]]; then
          sed -i 's/"cniVersion"[[:space:]]*:[[:space:]]*"1\.0\.0"/"cniVersion": "0.4.0"/g' "$f"
        else
          sudo sed -i 's/"cniVersion"[[:space:]]*:[[:space:]]*"1\.0\.0"/"cniVersion": "0.4.0"/g' "$f"
        fi
      fi
    done
  done
}

compose_cmd() {
  if can_use_podman_compose; then
    if command -v podman-compose >/dev/null 2>&1; then
      podman_rootful podman-compose "$@"
    elif podman_rootful podman compose version >/dev/null 2>&1; then
      podman_rootful podman compose "$@"
    else
      echo "error: can_use_podman_compose is true but neither podman-compose nor \`podman compose\` is available" >&2
      exit 1
    fi
    return
  fi
  if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    docker compose "$@"
    return
  fi
  if command -v docker-compose >/dev/null 2>&1; then
    docker-compose "$@"
    return
  fi
  echo "error: no compose command found after ensure_container_runtime" >&2
  exit 1
}

load_prebuilt_image_if_local() {
  [[ "${MERVYN_DEPLOY_MODE:-}" == "local" ]] || return 0
  local t="${HOME}/mervyn-image.tar"
  if [[ ! -f "$t" ]]; then
    echo "error: expected ${t} (from --local-build deploy) but it is missing" >&2
    exit 1
  fi
  if can_use_podman_compose; then
    podman_rootful podman load -i "$t"
  elif command -v docker >/dev/null 2>&1; then
    if docker compose version >/dev/null 2>&1 || command -v docker-compose >/dev/null 2>&1; then
      docker load -i "$t"
    else
      echo "error: cannot load mervyn-image.tar: no podman or docker compose stack" >&2
      exit 1
    fi
  else
    echo "error: cannot load mervyn-image.tar: need podman or docker" >&2
    exit 1
  fi
  rm -f "$t"
}

ensure_container_runtime
load_prebuilt_image_if_local
cd "${HOME}/mervyn"
tar xzf "${HOME}/mervyn-deploy.tgz"
rm -f "${HOME}/mervyn-deploy.tgz"
maybe_fix_podman_cni_conflists
if [[ "${MERVYN_DEPLOY_MODE:-}" == "local" ]]; then
  compose_cmd up -d
else
  compose_cmd up -d --build
fi
compose_cmd ps
