#!/usr/bin/env bash
# Bootstrap Mervyn on Oracle Cloud (Ampere A1) using the OCI CLI — similar in spirit to
# a one-shot gcloud run: network + Ubuntu ARM + Docker cloud-init, using your configured profile.
#
# Prerequisites (install once):
#   - Oracle Cloud Infrastructure CLI: https://docs.oracle.com/en-us/iaas/Content/API/SDKDocs/cliinstall.htm
#   - jq: https://jqlang.github.io/jq/
#
# One-time auth (pick one):
#   oci setup config     # writes ~/.oci/config + API key
#   # or: oci session authenticate --region <region>   # browser / token flow, if you use that
#
# Required: an API key (or session) that can manage networking and compute in the target compartment.
#
# Environment (optional):
#   OCI_CLI_PROFILE           Profile name in ~/.oci/config (default: DEFAULT)
#   OCI_CLI_CONFIG_FILE       Config path (default: ~/.oci/config)
#   MERVYN_PROJECT_NAME       Resource name prefix (default: mervyn)
#   MERVYN_COMPARTMENT_OCID   Compartment for VCN + VM (default: detect * (root) compartment)
#   MERVYN_SSH_PUBLIC_KEY     Full public key string (else use MERVYN_SSH_PUBLIC_KEY_FILE)
#   MERVYN_SSH_PUBLIC_KEY_FILE  Path to .pub (default: ~/.ssh/id_ed25519.pub or id_rsa.pub)
#   MERVYN_AD_INDEX           Availability domain index 0..n-1 (default: 0)
#   MERVYN_OCPUS              A1.Flex OCPUs (default: 1)
#   MERVYN_MEMORY_GBS         A1.Flex memory in GB (default: 6)
#   MERVYN_UBUNTU_VERSION     e.g. 22.04 (default: 22.04)
#   MERVYN_SSH_ALLOWED_CIDRS  Comma-separated CIDRs for SSH (default: 0.0.0.0/0)
#   MERVYN_HTTP_CIDRS         Comma-separated CIDRs for 80/443 (default: 0.0.0.0/0)
#   MERVYN_EXPOSE_APP_PORT    If true, also open TCP 3000 from MERVYN_APP_PORT_CIDRS (default: false)
#   MERVYN_APP_PORT_CIDRS     Comma-separated (default: 0.0.0.0/0)
#   MERVYN_BOOTSTRAP_DOCKER   If true, pass cloud-init Docker install (default: true)
#
# Example:
#   export MERVYN_SSH_ALLOWED_CIDRS="$(curl -sS https://checkip.amazonaws.com)/32"
#   ./scripts/oci-bootstrap.sh

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
CLOUD_INIT_FILE="${MERVYN_CLOUD_INIT_FILE:-$REPO_ROOT/terraform/cloud-init-docker.yaml}"

OCI_CONFIG="${OCI_CLI_CONFIG_FILE:-$HOME/.oci/config}"
OCI_PROFILE="${OCI_CLI_PROFILE:-DEFAULT}"

PROJECT="${MERVYN_PROJECT_NAME:-mervyn}"
AD_INDEX="${MERVYN_AD_INDEX:-0}"
OCPUS="${MERVYN_OCPUS:-1}"
MEM_GB="${MERVYN_MEMORY_GBS:-6}"
UBUNTU_VER="${MERVYN_UBUNTU_VERSION:-22.04}"
SSH_CIDRS_RAW="${MERVYN_SSH_ALLOWED_CIDRS:-0.0.0.0/0}"
HTTP_CIDRS_RAW="${MERVYN_HTTP_CIDRS:-0.0.0.0/0}"
APP_PORT_CIDRS_RAW="${MERVYN_APP_PORT_CIDRS:-0.0.0.0/0}"
EXPOSE_APP="${MERVYN_EXPOSE_APP_PORT:-false}"
BOOTSTRAP_DOCKER="${MERVYN_BOOTSTRAP_DOCKER:-true}"

for cmd in oci jq; do
  command -v "$cmd" >/dev/null 2>&1 || {
    echo "error: missing '$cmd' in PATH" >&2
    exit 1
  }
done

if [[ ! -f "$OCI_CONFIG" ]]; then
  echo "error: OCI config not found at $OCI_CONFIG (run: oci setup config)" >&2
  exit 1
fi

get_ini() {
  local key=$1
  awk -v profile="$OCI_PROFILE" -v k="$key" '
    $0 ~ "^\\[" profile "\\]" { insec = 1; next }
    /^\[/ { insec = 0 }
    insec && $0 ~ "^" k "=" {
      sub(/^[^=]+=/, "", $0)
      print
      exit
    }
  ' "$OCI_CONFIG" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'
}

TENANCY_OCID=$(get_ini tenancy)
REGION=$(get_ini region)

if [[ -z "${TENANCY_OCID:-}" || -z "${REGION:-}" ]]; then
  echo "error: could not read tenancy= and region= from [$OCI_PROFILE] in $OCI_CONFIG" >&2
  exit 1
fi

export OCI_CLI_PROFILE="$OCI_PROFILE"

oci_args=(--config-file "$OCI_CONFIG" --profile "$OCI_PROFILE" --region "$REGION")

resolve_ssh_key() {
  if [[ -n "${MERVYN_SSH_PUBLIC_KEY:-}" ]]; then
    printf '%s' "$MERVYN_SSH_PUBLIC_KEY"
    return
  fi
  local f="${MERVYN_SSH_PUBLIC_KEY_FILE:-}"
  if [[ -z "$f" ]]; then
    if [[ -f "$HOME/.ssh/id_ed25519.pub" ]]; then
      f="$HOME/.ssh/id_ed25519.pub"
    elif [[ -f "$HOME/.ssh/id_rsa.pub" ]]; then
      f="$HOME/.ssh/id_rsa.pub"
    else
      echo "error: set MERVYN_SSH_PUBLIC_KEY or MERVYN_SSH_PUBLIC_KEY_FILE, or add ~/.ssh/id_ed25519.pub" >&2
      exit 1
    fi
  fi
  if [[ ! -f "$f" ]]; then
    echo "error: SSH public key file not found: $f" >&2
    exit 1
  fi
  tr -d '\r\n' <"$f"
}

SSH_PUB=$(resolve_ssh_key)

resolve_compartment() {
  if [[ -n "${MERVYN_COMPARTMENT_OCID:-}" ]]; then
    printf '%s' "$MERVYN_COMPARTMENT_OCID"
    return
  fi
  local root_id
  root_id=$(oci "${oci_args[@]}" iam compartment list \
    --compartment-id "$TENANCY_OCID" \
    --all \
    --query "data[?contains(name, '(root)')].id | [0]" \
    --raw-output 2>/dev/null || true)
  if [[ -n "$root_id" && "$root_id" != "null" ]]; then
    printf '%s' "$root_id"
    return
  fi
  echo "warn: could not find a compartment named like *(root)*; using tenancy OCID as compartment (may fail in some tenancies)" >&2
  printf '%s' "$TENANCY_OCID"
}

COMPARTMENT_OCID=$(resolve_compartment)

split_csv_json() {
  jq -n --arg s "$1" '$s | split(",") | map(gsub("^\\s+|\\s+$"; "")) | map(select(length > 0))'
}

SSH_CIDRS_JSON=$(split_csv_json "$SSH_CIDRS_RAW")
HTTP_CIDRS_JSON=$(split_csv_json "$HTTP_CIDRS_RAW")
APP_CIDRS_JSON=$(split_csv_json "$APP_PORT_CIDRS_RAW")

INGRESS_SSH=$(jq -n --argjson cidrs "$SSH_CIDRS_JSON" '
  $cidrs | map({
    protocol: "6",
    source: .,
    tcpOptions: { destinationPortRange: { min: 22, max: 22 } }
  })
')

INGRESS_HTTP=$(jq -n --argjson cidrs "$HTTP_CIDRS_JSON" '
  $cidrs
  | map(. as $c
    | [
        { protocol: "6", source: $c, tcpOptions: { destinationPortRange: { min: 80, max: 80 } } },
        { protocol: "6", source: $c, tcpOptions: { destinationPortRange: { min: 443, max: 443 } } }
      ]
    )
  | add
')

INGRESS_RULES_JSON=$INGRESS_SSH
INGRESS_RULES_JSON=$(jq -n --argjson a "$INGRESS_RULES_JSON" --argjson b "$INGRESS_HTTP" '$a + $b')

if [[ "$EXPOSE_APP" == "true" || "$EXPOSE_APP" == "1" ]]; then
  INGRESS_APP=$(jq -n --argjson cidrs "$APP_CIDRS_JSON" '
    $cidrs | map({
      protocol: "6",
      source: .,
      tcpOptions: { destinationPortRange: { min: 3000, max: 3000 } }
    })
  ')
  INGRESS_RULES_JSON=$(jq -n --argjson a "$INGRESS_RULES_JSON" --argjson b "$INGRESS_APP" '$a + $b')
fi

EGRESS_RULES_JSON='[{"protocol":"all","destination":"0.0.0.0/0"}]'

dns_label_vcn=$(echo "$PROJECT" | tr -d '-')
dns_label_subnet="${dns_label_vcn}pub"
if [[ ${#dns_label_subnet} -gt 15 ]]; then
  dns_label_subnet="${dns_label_subnet:0:15}"
fi

echo "==> Using profile=$OCI_PROFILE region=$REGION compartment=$COMPARTMENT_OCID"

AD_NAME=$(oci "${oci_args[@]}" iam availability-domain list \
  --compartment-id "$TENANCY_OCID" \
  --query "data[$AD_INDEX].name" \
  --raw-output)

if [[ -z "$AD_NAME" || "$AD_NAME" == "null" ]]; then
  echo "error: no availability domain at index $AD_INDEX (try MERVYN_AD_INDEX=1)" >&2
  exit 1
fi
echo "==> Availability domain: $AD_NAME"

echo "==> Resolving Ubuntu ${UBUNTU_VER} aarch64 image for VM.Standard.A1.Flex..."
IMAGES_JSON=$(oci "${oci_args[@]}" compute image list \
  --compartment-id "$COMPARTMENT_OCID" \
  --operating-system "Canonical Ubuntu" \
  --operating-system-version "$UBUNTU_VER" \
  --shape "VM.Standard.A1.Flex" \
  --sort-by TIMECREATED \
  --sort-order DESC \
  --all \
  --output json)

IMAGE_ID=$(echo "$IMAGES_JSON" | jq -r '
  .data as $d
  | ($d | map(select(
      (."display-name" // .displayName // "") | ascii_downcase | contains("minimal") | not
    ))) as $nm
  | if ($nm | length) > 0 then $nm[0].id else ($d[0].id // empty) end
')
DISPLAY_PICKED=$(echo "$IMAGES_JSON" | jq -r --arg id "$IMAGE_ID" '
  .data[] | select(.id == $id) | (."display-name" // .displayName // "")
')

if [[ -n "$DISPLAY_PICKED" ]]; then
  echo "    image: $DISPLAY_PICKED"
fi

if [[ -z "$IMAGE_ID" || "$IMAGE_ID" == "null" ]]; then
  echo "error: no compatible Ubuntu image found (region/A1 capacity/shape filter)" >&2
  exit 1
fi

echo "==> Creating VCN ${PROJECT}-vcn..."
VCN_JSON=$(oci "${oci_args[@]}" network vcn create \
  --cidr-block 10.0.0.0/16 \
  --compartment-id "$COMPARTMENT_OCID" \
  --display-name "${PROJECT}-vcn" \
  --dns-label "$dns_label_vcn" \
  --wait-for-state AVAILABLE \
  --output json)

VCN_ID=$(echo "$VCN_JSON" | jq -r '.data.id')
RT_ID=$(echo "$VCN_JSON" | jq -r '.data["default-route-table-id"] // .data.defaultRouteTableId')

echo "==> Creating internet gateway..."
IGW_JSON=$(oci "${oci_args[@]}" network internet-gateway create \
  --compartment-id "$COMPARTMENT_OCID" \
  --vcn-id "$VCN_ID" \
  --display-name "${PROJECT}-igw" \
  --is-enabled true \
  --wait-for-state AVAILABLE \
  --output json)
IGW_ID=$(echo "$IGW_JSON" | jq -r '.data.id')

echo "==> Updating default route table to use internet gateway..."
oci "${oci_args[@]}" network route-table update \
  --rt-id "$RT_ID" \
  --route-rules "[{\"destination\":\"0.0.0.0/0\",\"destinationType\":\"CIDR_BLOCK\",\"networkEntityId\":\"$IGW_ID\"}]" \
  --force \
  --wait-for-state AVAILABLE \
  >/dev/null

echo "==> Creating security list ${PROJECT}-public-sl..."
SL_JSON=$(oci "${oci_args[@]}" network security-list create \
  --compartment-id "$COMPARTMENT_OCID" \
  --vcn-id "$VCN_ID" \
  --display-name "${PROJECT}-public-sl" \
  --ingress-security-rules "$INGRESS_RULES_JSON" \
  --egress-security-rules "$EGRESS_RULES_JSON" \
  --wait-for-state AVAILABLE \
  --output json)
SL_ID=$(echo "$SL_JSON" | jq -r '.data.id')

echo "==> Creating public subnet..."
SUBNET_JSON=$(oci "${oci_args[@]}" network subnet create \
  --cidr-block 10.0.0.0/24 \
  --compartment-id "$COMPARTMENT_OCID" \
  --vcn-id "$VCN_ID" \
  --display-name "${PROJECT}-public" \
  --dns-label "$dns_label_subnet" \
  --prohibit-public-ip-on-vnic false \
  --route-table-id "$RT_ID" \
  --security-list-ids "[\"$SL_ID\"]" \
  --wait-for-state AVAILABLE \
  --output json)
SUBNET_ID=$(echo "$SUBNET_JSON" | jq -r '.data.id')

METADATA_JSON=$(jq -n --arg ssh "$SSH_PUB" '{ssh_authorized_keys: $ssh}')
if [[ "$BOOTSTRAP_DOCKER" == "true" || "$BOOTSTRAP_DOCKER" == "1" ]]; then
  if [[ ! -f "$CLOUD_INIT_FILE" ]]; then
    echo "error: cloud-init file missing: $CLOUD_INIT_FILE" >&2
    exit 1
  fi
  UD_B64=$(base64 <"$CLOUD_INIT_FILE" | tr -d '\n')
  METADATA_JSON=$(echo "$METADATA_JSON" | jq --arg ud "$UD_B64" '. + {user_data: $ud}')
fi

META_FILE=$(mktemp)
SHAPE_FILE=$(mktemp)
cleanup_tmp() {
  rm -f "$META_FILE" "$SHAPE_FILE"
}
trap cleanup_tmp EXIT
printf '%s' "$METADATA_JSON" >"$META_FILE"

SHAPE_CONFIG_JSON=$(jq -n --argjson o "$OCPUS" --argjson m "$MEM_GB" '{ocpus: $o, memoryInGBs: $m}')
printf '%s' "$SHAPE_CONFIG_JSON" >"$SHAPE_FILE"

echo "==> Launching instance ${PROJECT}-arm (shape VM.Standard.A1.Flex)..."
INST_JSON=$(oci "${oci_args[@]}" compute instance launch \
  --availability-domain "$AD_NAME" \
  --compartment-id "$COMPARTMENT_OCID" \
  --shape "VM.Standard.A1.Flex" \
  --shape-config "file://$SHAPE_FILE" \
  --display-name "${PROJECT}-arm" \
  --subnet-id "$SUBNET_ID" \
  --assign-public-ip true \
  --image-id "$IMAGE_ID" \
  --metadata "file://$META_FILE" \
  --wait-for-state RUNNING \
  --output json)

IID=$(echo "$INST_JSON" | jq -r '.data.id')
echo "    instance OCID: $IID"

echo "==> Waiting for public IP on primary VNIC..."
PUBLIC_IP=""
for _ in $(seq 1 30); do
  VNIC_JSON=$(oci "${oci_args[@]}" compute instance list-vnics --instance-id "$IID" --output json)
  PUBLIC_IP=$(echo "$VNIC_JSON" | jq -r '.data[0]."public-ip" // .data[0].publicIp // empty')
  if [[ -n "$PUBLIC_IP" && "$PUBLIC_IP" != "null" ]]; then
    break
  fi
  sleep 2
done

if [[ -z "$PUBLIC_IP" || "$PUBLIC_IP" == "null" ]]; then
  echo "warn: public IP not visible yet; check later: oci compute instance list-vnics --instance-id $IID" >&2
else
  echo ""
  echo "Done."
  echo "  Public IP:  $PUBLIC_IP"
  echo "  SSH:        ssh ubuntu@$PUBLIC_IP"
  echo ""
  echo "Next: copy repo + .env, mkdir -p data/vault, put TLS on 443 for Slack (see README)."
fi
