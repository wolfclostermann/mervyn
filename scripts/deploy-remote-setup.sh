#!/usr/bin/env bash
# Run on the deployment VM only (invoked via SSH by deploy-gcp.sh / deploy-oci.sh).
set -euo pipefail
export PATH="${HOME}/.local/bin:${PATH}"

# Non-secret hints from terraform.tfvars (written by deploy-oci.sh as ~/mervyn-deploy-config.env).
if [[ -f "${HOME}/mervyn-deploy-config.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "${HOME}/mervyn-deploy-config.env"
  set +a
  rm -f "${HOME}/mervyn-deploy-config.env"
fi

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
# Oracle Linux: upgrade podman from dnf when major < 4.
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
  [[ -r /etc/os-release ]] || return 0
  # shellcheck disable=SC1091
  . /etc/os-release

  if [[ "${ID:-}" == "ubuntu" ]] && [[ "${VERSION_ID:-}" == "22.04" || "${VERSION_ID:-}" == "24.04" ]]; then
    local major=0
    if command -v podman >/dev/null 2>&1; then
      major=$(podman --version 2>/dev/null | awk '{print $3}' | cut -d. -f1)
      major=${major:-0}
    fi
    [[ "${major:-0}" -lt 4 ]] || return 0

    echo "[deploy-remote-setup] Podman < 4 on Ubuntu ${VERSION_ID}: adding Kubic repo and upgrading Podman (for reliable builds)..." >&2
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl gnupg
    sudo mkdir -p /etc/apt/keyrings
    sudo rm -f /etc/apt/keyrings/devel_kubic_libcontainers_stable.gpg
    curl -fsSL "https://download.opensuse.org/repositories/devel:/kubic:/libcontainers:/stable/xUbuntu_${VERSION_ID}/Release.key" | sudo gpg --batch --no-tty --dearmor -o /etc/apt/keyrings/devel_kubic_libcontainers_stable.gpg
    echo "deb [signed-by=/etc/apt/keyrings/devel_kubic_libcontainers_stable.gpg] https://download.opensuse.org/repositories/devel:/kubic:/libcontainers:/stable/xUbuntu_${VERSION_ID}/ /" | sudo tee /etc/apt/sources.list.d/devel:kubic:libcontainers:stable.list >/dev/null
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y podman
    return 0
  fi

  if [[ "${ID:-}" == "ol" ]]; then
    command -v dnf >/dev/null 2>&1 || return 0
    # Fresh OL has no podman: skip upgrade (dnf metadata alone can OOM on 1GB shapes).
    command -v podman >/dev/null 2>&1 || return 0
    local maj=0
    maj=$(podman --version 2>/dev/null | awk '{print $3}' | cut -d. -f1)
    maj=${maj:-0}
    [[ "${maj:-0}" -lt 4 ]] || return 0
    echo "[deploy-remote-setup] upgrading Podman via dnf (existing install is major ${maj})..." >&2
    sudo dnf upgrade -y podman 2>/dev/null || true
    return 0
  fi

  return 0
}

_dnf_ol_bail() {
  echo "error: dnf install failed (${1:-packages}). If you saw 'Killed', Linux OOM-killer likely stopped dnf — common on 1GB VMs." >&2
  echo "  Fix: add swap, use a larger shape, or from your laptop run ./scripts/deploy-oci.sh --local-build (smaller peak RAM on the VM)." >&2
  exit 1
}

ensure_container_runtime() {
  [[ -r /etc/os-release ]] || {
    echo "error: /etc/os-release missing; cannot install Podman automatically." >&2
    exit 1
  }
  # shellcheck disable=SC1091
  . /etc/os-release

  echo "[deploy-remote-setup] host: ${PRETTY_NAME:-${ID:-unknown} ${VERSION_ID:-}}; ensuring Podman and Compose..." >&2

  # Pre-built image: no remote multi-stage build; stock Podman 3 can still load and run the image.
  if [[ "${MERVYN_DEPLOY_MODE:-}" != "local" ]]; then
    echo "[deploy-remote-setup] checking whether Podman needs an upgrade for remote image builds..." >&2
    ensure_podman_recent_for_multistage_build
  else
    echo "[deploy-remote-setup] MERVYN_DEPLOY_MODE=local — skipping Podman upgrade step." >&2
  fi
  if has_working_compose; then
    echo "[deploy-remote-setup] Podman/Docker Compose already usable; skipping package installs." >&2
    return 0
  fi

  case "${ID:-}" in
    ubuntu)
      echo "[deploy-remote-setup] installing Podman stack (apt) and pip podman-compose..." >&2
      sudo apt-get update -qq
      if ! command -v podman >/dev/null 2>&1; then
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -y podman fuse-overlayfs slirp4netns uidmap iptables
      fi
      sudo DEBIAN_FRONTEND=noninteractive apt-get install -y python3-pip
      echo "[deploy-remote-setup] pip install --user podman-compose..." >&2
      python3 -m pip install --user podman-compose
      ;;
    ol)
      echo "[deploy-remote-setup] installing Podman via dnf (two steps to reduce peak RAM on small instances)..." >&2
      sudo dnf install -y podman || _dnf_ol_bail podman
      sudo dnf install -y fuse-overlayfs slirp4netns iptables python3-pip || _dnf_ol_bail "fuse-overlayfs, slirp4netns, iptables, python3-pip"
      hash -r 2>/dev/null || true
      export PATH="${HOME}/.local/bin:${PATH}"
      if can_use_podman_compose; then
        echo "[deploy-remote-setup] podman compose already available after dnf install." >&2
        return 0
      fi
      echo "[deploy-remote-setup] pip install --user podman-compose..." >&2
      python3 -m pip install --user podman-compose 2>/dev/null || python3 -m pip install --break-system-packages --user podman-compose
      ;;
    *)
      echo "error: unsupported OS '${ID:-unknown}' (automated Podman install supports ubuntu and ol)." >&2
      exit 1
      ;;
  esac

  hash -r 2>/dev/null || true
  export PATH="${HOME}/.local/bin:${PATH}"
  if can_use_podman_compose; then
    return 0
  fi
  if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    return 0
  fi
  if command -v docker-compose >/dev/null 2>&1; then
    return 0
  fi
  echo "error: could not install Podman Compose (podman-compose or \`podman compose\`)." >&2
  exit 1
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
  local dir=/etc/cni/net.d f
  [[ -d "$dir" ]] || return 0
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
}

compose_cmd() {
  echo "[deploy-remote-setup] compose $*" >&2
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
  echo "[deploy-remote-setup] loading container image from $(basename "$t") into Podman..." >&2
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

echo "[deploy-remote-setup] starting (user=$(whoami), home=${HOME})..." >&2

ensure_container_runtime
load_prebuilt_image_if_local

echo "[deploy-remote-setup] extracting application bundle to ${HOME}/mervyn ..." >&2
cd "${HOME}/mervyn"
tar xzf "${HOME}/mervyn-deploy.tgz"
rm -f "${HOME}/mervyn-deploy.tgz"

echo "[deploy-remote-setup] fixing Podman CNI conflist versions if needed..." >&2
maybe_fix_podman_cni_conflists

if [[ "${MERVYN_DEPLOY_MODE:-}" == "local" ]]; then
  echo "[deploy-remote-setup] compose up -d (pre-built image, no --build)..." >&2
  compose_cmd up -d
else
  echo "[deploy-remote-setup] compose up -d --build (Rust/image build can take many minutes; output may be sparse)..." >&2
  compose_cmd up -d --build
fi

echo "[deploy-remote-setup] running compose ps:" >&2
compose_cmd ps
echo "[deploy-remote-setup] done." >&2
