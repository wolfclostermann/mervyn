#!/usr/bin/env bash
# Bootstrap Mervyn on Oracle Cloud (default: Ampere VM.Standard.A1.Flex) using the OCI CLI —
# similar in spirit to a one-shot gcloud run: network + Ubuntu + Docker cloud-init.
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
#   MERVYN_AD_INDEX           First availability domain to try, 0..n-1 (default: 0);
#                             on "Out of host capacity" the script tries other ADs in order.
#   MERVYN_COMPUTE_SHAPE        e.g. VM.Standard.A1.Flex (default Ampere) or VM.Standard.E2.1.Micro
#   MERVYN_INSTANCE_DISPLAY_NAME  Compute display name for idempotency (default: ${PROJECT}-arm)
#   MERVYN_OCPUS              Flex shapes only: OCPUs (default: 1)
#   MERVYN_MEMORY_GBS         Flex shapes only: memory in GB (default: 6)
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
#
# Re-runs: networking is reused when resources with the expected display names already exist
# in the compartment. Instance launch is skipped if a RUNNING/PROVISIONING/STARTING instance
# named MERVYN_INSTANCE_DISPLAY_NAME (default ${MERVYN_PROJECT_NAME:-mervyn}-arm) already exists.
# Otherwise launch tries each
# availability domain in rotation until success or a non-capacity error.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
CLOUD_INIT_FILE="${MERVYN_CLOUD_INIT_FILE:-$REPO_ROOT/terraform/cloud-init-docker.yaml}"

OCI_CONFIG="${OCI_CLI_CONFIG_FILE:-$HOME/.oci/config}"
OCI_PROFILE="${OCI_CLI_PROFILE:-DEFAULT}"

PROJECT="${MERVYN_PROJECT_NAME:-mervyn}"
COMPUTE_SHAPE="${MERVYN_COMPUTE_SHAPE:-VM.Standard.A1.Flex}"
INST_DN="${MERVYN_INSTANCE_DISPLAY_NAME:-${PROJECT}-arm}"
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

shape_requires_flex_config() {
  case "$1" in
  *Flex) return 0 ;;
  *) return 1 ;;
  esac
}

echo "==> Resolving Ubuntu ${UBUNTU_VER} image compatible with $COMPUTE_SHAPE..."
IMAGES_JSON=$(oci "${oci_args[@]}" compute image list \
  --compartment-id "$COMPARTMENT_OCID" \
  --operating-system "Canonical Ubuntu" \
  --operating-system-version "$UBUNTU_VER" \
  --shape "$COMPUTE_SHAPE" \
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
  echo "error: no compatible Ubuntu image found for $COMPUTE_SHAPE (region or shape filter)" >&2
  exit 1
fi

# --- Networking (idempotent: reuse by display-name when already AVAILABLE) ---
VCN_DN="${PROJECT}-vcn"
IGW_DN="${PROJECT}-igw"
SL_DN="${PROJECT}-public-sl"
SUBNET_DN="${PROJECT}-public"

jq_vcn_match() {
  jq -r --arg dn "$VCN_DN" '
    .data[]?
    | select(
        (."display-name" // .displayName // "") == $dn
        and (."lifecycle-state" // .lifecycleState // "") == "AVAILABLE"
      )
    | .id // empty' | head -1
}

VCN_ID=$(oci "${oci_args[@]}" network vcn list \
  --compartment-id "$COMPARTMENT_OCID" \
  --all \
  --output json | jq_vcn_match)

if [[ -n "$VCN_ID" ]]; then
  echo "==> Reusing existing VCN $VCN_DN"
  VCN_JSON=$(oci "${oci_args[@]}" network vcn get --vcn-id "$VCN_ID" --output json)
else
  echo "==> Creating VCN $VCN_DN..."
  set +e
  VCN_JSON=$(oci "${oci_args[@]}" network vcn create \
    --cidr-block 10.0.0.0/16 \
    --compartment-id "$COMPARTMENT_OCID" \
    --display-name "$VCN_DN" \
    --dns-label "$dns_label_vcn" \
    --wait-for-state AVAILABLE \
    --output json 2>&1)
  vcn_rc=$?
  set -e
  if [[ $vcn_rc -ne 0 ]]; then
    if echo "$VCN_JSON" | grep -qi 'already exists\|Duplicate\|Conflict\|not unique'; then
      VCN_ID=$(oci "${oci_args[@]}" network vcn list \
        --compartment-id "$COMPARTMENT_OCID" \
        --all \
        --output json | jq_vcn_match)
      if [[ -n "$VCN_ID" ]]; then
        echo "==> VCN create reported conflict; reusing $VCN_DN"
        VCN_JSON=$(oci "${oci_args[@]}" network vcn get --vcn-id "$VCN_ID" --output json)
      else
        echo "$VCN_JSON" >&2
        exit "$vcn_rc"
      fi
    else
      echo "$VCN_JSON" >&2
      exit "$vcn_rc"
    fi
  fi
fi

VCN_ID=$(echo "$VCN_JSON" | jq -r '.data.id')
RT_ID=$(echo "$VCN_JSON" | jq -r '.data["default-route-table-id"] // .data.defaultRouteTableId')

IGW_ID=$(oci "${oci_args[@]}" network internet-gateway list \
  --compartment-id "$COMPARTMENT_OCID" \
  --vcn-id "$VCN_ID" \
  --all \
  --output json | jq -r --arg dn "$IGW_DN" '
    .data[]?
    | select(
        (."display-name" // .displayName // "") == $dn
        and (."lifecycle-state" // .lifecycleState // "") == "AVAILABLE"
      )
    | .id // empty' | head -1)

if [[ -n "$IGW_ID" ]]; then
  echo "==> Reusing existing internet gateway $IGW_DN"
else
  echo "==> Creating internet gateway $IGW_DN..."
  set +e
  IGW_JSON=$(oci "${oci_args[@]}" network internet-gateway create \
    --compartment-id "$COMPARTMENT_OCID" \
    --vcn-id "$VCN_ID" \
    --display-name "$IGW_DN" \
    --is-enabled true \
    --wait-for-state AVAILABLE \
    --output json 2>&1)
  igw_rc=$?
  set -e
  if [[ $igw_rc -ne 0 ]]; then
    if echo "$IGW_JSON" | grep -qi 'already exists\|Duplicate\|Conflict\|not unique'; then
      IGW_ID=$(oci "${oci_args[@]}" network internet-gateway list \
        --compartment-id "$COMPARTMENT_OCID" \
        --vcn-id "$VCN_ID" \
        --all \
        --output json | jq -r --arg dn "$IGW_DN" '
          .data[]?
          | select((."display-name" // .displayName // "") == $dn)
          | .id // empty' | head -1)
      if [[ -z "$IGW_ID" ]]; then
        echo "$IGW_JSON" >&2
        exit "$igw_rc"
      fi
      echo "==> Internet gateway create reported conflict; reusing $IGW_DN"
    else
      echo "$IGW_JSON" >&2
      exit "$igw_rc"
    fi
  else
    IGW_ID=$(echo "$IGW_JSON" | jq -r '.data.id')
  fi
fi

echo "==> Ensuring default route table sends 0.0.0.0/0 to internet gateway..."
oci "${oci_args[@]}" network route-table update \
  --rt-id "$RT_ID" \
  --route-rules "[{\"destination\":\"0.0.0.0/0\",\"destinationType\":\"CIDR_BLOCK\",\"networkEntityId\":\"$IGW_ID\"}]" \
  --force \
  --wait-for-state AVAILABLE \
  >/dev/null

SL_ID=$(oci "${oci_args[@]}" network security-list list \
  --compartment-id "$COMPARTMENT_OCID" \
  --vcn-id "$VCN_ID" \
  --all \
  --output json | jq -r --arg dn "$SL_DN" '
    .data[]?
    | select(
        (."display-name" // .displayName // "") == $dn
        and (."lifecycle-state" // .lifecycleState // "") == "AVAILABLE"
      )
    | .id // empty' | head -1)

if [[ -n "$SL_ID" ]]; then
  echo "==> Updating existing security list $SL_DN (ingress/egress rules)"
  oci "${oci_args[@]}" network security-list update \
    --security-list-id "$SL_ID" \
    --ingress-security-rules "$INGRESS_RULES_JSON" \
    --egress-security-rules "$EGRESS_RULES_JSON" \
    --force \
    --wait-for-state AVAILABLE \
    >/dev/null
else
  echo "==> Creating security list $SL_DN..."
  set +e
  SL_JSON=$(oci "${oci_args[@]}" network security-list create \
    --compartment-id "$COMPARTMENT_OCID" \
    --vcn-id "$VCN_ID" \
    --display-name "$SL_DN" \
    --ingress-security-rules "$INGRESS_RULES_JSON" \
    --egress-security-rules "$EGRESS_RULES_JSON" \
    --wait-for-state AVAILABLE \
    --output json 2>&1)
  sl_rc=$?
  set -e
  if [[ $sl_rc -ne 0 ]]; then
    if echo "$SL_JSON" | grep -qi 'already exists\|Duplicate\|Conflict\|not unique'; then
      SL_ID=$(oci "${oci_args[@]}" network security-list list \
        --compartment-id "$COMPARTMENT_OCID" \
        --vcn-id "$VCN_ID" \
        --all \
        --output json | jq -r --arg dn "$SL_DN" '
          .data[]? | select((."display-name" // .displayName // "") == $dn) | .id // empty' | head -1)
      if [[ -n "$SL_ID" ]]; then
        echo "==> Security list create reported conflict; updating $SL_DN"
        oci "${oci_args[@]}" network security-list update \
          --security-list-id "$SL_ID" \
          --ingress-security-rules "$INGRESS_RULES_JSON" \
          --egress-security-rules "$EGRESS_RULES_JSON" \
          --force \
          --wait-for-state AVAILABLE \
          >/dev/null
      else
        echo "$SL_JSON" >&2
        exit "$sl_rc"
      fi
    else
      echo "$SL_JSON" >&2
      exit "$sl_rc"
    fi
  else
    SL_ID=$(echo "$SL_JSON" | jq -r '.data.id')
  fi
fi

SUBNET_ID=$(oci "${oci_args[@]}" network subnet list \
  --compartment-id "$COMPARTMENT_OCID" \
  --vcn-id "$VCN_ID" \
  --all \
  --output json | jq -r --arg dn "$SUBNET_DN" '
    .data[]?
    | select(
        (."display-name" // .displayName // "") == $dn
        and (."lifecycle-state" // .lifecycleState // "") == "AVAILABLE"
      )
    | .id // empty' | head -1)

if [[ -n "$SUBNET_ID" ]]; then
  echo "==> Reusing existing subnet $SUBNET_DN"
else
  echo "==> Creating public subnet $SUBNET_DN..."
  set +e
  SUBNET_JSON=$(oci "${oci_args[@]}" network subnet create \
    --cidr-block 10.0.0.0/24 \
    --compartment-id "$COMPARTMENT_OCID" \
    --vcn-id "$VCN_ID" \
    --display-name "$SUBNET_DN" \
    --dns-label "$dns_label_subnet" \
    --prohibit-public-ip-on-vnic false \
    --route-table-id "$RT_ID" \
    --security-list-ids "[\"$SL_ID\"]" \
    --wait-for-state AVAILABLE \
    --output json 2>&1)
  sn_rc=$?
  set -e
  if [[ $sn_rc -ne 0 ]]; then
    if echo "$SUBNET_JSON" | grep -qi 'already exists\|Duplicate\|Conflict\|not unique'; then
      SUBNET_ID=$(oci "${oci_args[@]}" network subnet list \
        --compartment-id "$COMPARTMENT_OCID" \
        --vcn-id "$VCN_ID" \
        --all \
        --output json | jq -r --arg dn "$SUBNET_DN" '
          .data[]? | select((."display-name" // .displayName // "") == $dn) | .id // empty' | head -1)
      if [[ -z "$SUBNET_ID" ]]; then
        echo "$SUBNET_JSON" >&2
        exit "$sn_rc"
      fi
      echo "==> Subnet create reported conflict; reusing $SUBNET_DN"
    else
      echo "$SUBNET_JSON" >&2
      exit "$sn_rc"
    fi
  else
    SUBNET_ID=$(echo "$SUBNET_JSON" | jq -r '.data.id')
  fi
fi

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
SHAPE_FILE=""
if shape_requires_flex_config "$COMPUTE_SHAPE"; then
  SHAPE_FILE=$(mktemp)
  SHAPE_CONFIG_JSON=$(jq -n --argjson o "$OCPUS" --argjson m "$MEM_GB" '{ocpus: $o, memoryInGBs: $m}')
  printf '%s' "$SHAPE_CONFIG_JSON" >"$SHAPE_FILE"
fi
cleanup_tmp() {
  rm -f "$META_FILE"
  [[ -n "$SHAPE_FILE" ]] && rm -f "$SHAPE_FILE"
}
trap cleanup_tmp EXIT
printf '%s' "$METADATA_JSON" >"$META_FILE"

# True when launch failed in a way that may succeed in another AD (capacity, unknown AD, etc.).
launch_error_try_next_ad() {
  local text=$1
  echo "$text" | grep -qi 'Out of host capacity' && return 0
  echo "$text" | grep -qi 'InsufficientHostCapacity' && return 0
  echo "$text" | grep -qi 'no host capacity' && return 0
  # Some regions list AD names that compute no longer accepts, or fewer ADs than IAM returns.
  echo "$text" | grep -qi 'not found' && return 0
  echo "$text" | grep -qi 'NotFound' && return 0
  echo "$text" | grep -qi 'UnknownAvailabilityDomain' && return 0
  echo "$text" | grep -qi 'Invalid availability domain' && return 0
  return 1
}

AD_JSON=$(oci "${oci_args[@]}" iam availability-domain list \
  --compartment-id "$TENANCY_OCID" \
  --output json)
n_ads=$(echo "$AD_JSON" | jq '.data | length')
if [[ "$n_ads" -lt 1 ]]; then
  echo "error: no availability domains in region $REGION" >&2
  exit 1
fi
if [[ "$AD_INDEX" -ge "$n_ads" || "$AD_INDEX" -lt 0 ]]; then
  echo "warn: MERVYN_AD_INDEX=$AD_INDEX out of range 0..$((n_ads - 1)); using 0" >&2
  AD_INDEX=0
fi

declare -a AD_ORDERED=()
for ((offset = 0; offset < n_ads; offset++)); do
  i=$(((AD_INDEX + offset) % n_ads))
  nm=$(echo "$AD_JSON" | jq -r ".data[$i].name // empty")
  [[ -z "$nm" || "$nm" == "null" ]] && continue
  dup=0
  for ((e = 0; e < ${#AD_ORDERED[@]}; e++)); do
    [[ "${AD_ORDERED[$e]}" == "$nm" ]] && {
      dup=1
      break
    }
  done
  [[ "$dup" -eq 1 ]] && continue
  AD_ORDERED+=("$nm")
done
if ((${#AD_ORDERED[@]} == 0)); then
  echo "error: could not resolve any availability domain names from IAM API" >&2
  exit 1
fi

EXIST_IID=$(oci "${oci_args[@]}" compute instance list \
  --compartment-id "$COMPARTMENT_OCID" \
  --all \
  --output json | jq -r --arg dn "$INST_DN" '
    .data[]?
    | select(
        (."display-name" // .displayName // "") == $dn
        and (
          (."lifecycle-state" // .lifecycleState // "") == "RUNNING"
          or (."lifecycle-state" // .lifecycleState // "") == "PROVISIONING"
          or (."lifecycle-state" // .lifecycleState // "") == "STARTING"
        )
      )
    | .id // empty' | head -1)

if [[ -n "$EXIST_IID" ]]; then
  echo "==> Reusing existing instance $INST_DN ($EXIST_IID)"
  IID=$EXIST_IID
  ls_state=$(oci "${oci_args[@]}" compute instance get --instance-id "$IID" --output json \
    | jq -r '.data["lifecycle-state"] // .data.lifecycleState // empty')
  if [[ "$ls_state" != "RUNNING" ]]; then
    echo "    waiting for RUNNING (currently $ls_state)..."
    oci "${oci_args[@]}" compute instance get \
      --instance-id "$IID" \
      --wait-for-state RUNNING \
      >/dev/null
  fi
else
  INST_JSON=""
  last_launch_err=""
  launched=0
  for ((ad_i = 0; ad_i < ${#AD_ORDERED[@]}; ad_i++)); do
    AD_NAME="${AD_ORDERED[$ad_i]}"
    echo "==> Launching instance $INST_DN in $AD_NAME (shape $COMPUTE_SHAPE)..."
    set +e
    if shape_requires_flex_config "$COMPUTE_SHAPE"; then
      out=$(oci "${oci_args[@]}" compute instance launch \
        --availability-domain "$AD_NAME" \
        --compartment-id "$COMPARTMENT_OCID" \
        --shape "$COMPUTE_SHAPE" \
        --shape-config "file://$SHAPE_FILE" \
        --display-name "$INST_DN" \
        --subnet-id "$SUBNET_ID" \
        --assign-public-ip true \
        --image-id "$IMAGE_ID" \
        --metadata "file://$META_FILE" \
        --wait-for-state RUNNING \
        --output json 2>&1)
    else
      out=$(oci "${oci_args[@]}" compute instance launch \
        --availability-domain "$AD_NAME" \
        --compartment-id "$COMPARTMENT_OCID" \
        --shape "$COMPUTE_SHAPE" \
        --display-name "$INST_DN" \
        --subnet-id "$SUBNET_ID" \
        --assign-public-ip true \
        --image-id "$IMAGE_ID" \
        --metadata "file://$META_FILE" \
        --wait-for-state RUNNING \
        --output json 2>&1)
    fi
    rc=$?
    set -e
    if [[ $rc -eq 0 ]]; then
      INST_JSON=$out
      launched=1
      break
    fi
    last_launch_err=$out
    if launch_error_try_next_ad "$out"; then
      echo "warn: $AD_NAME: skipped (capacity or AD not usable); trying next availability domain..." >&2
      continue
    fi
    echo "$out" >&2
    exit "$rc"
  done
  if [[ "$launched" -ne 1 ]]; then
    echo "error: instance launch failed in all ${#AD_ORDERED[@]} availability domain(s) tried" >&2
    echo "$last_launch_err" >&2
    exit 1
  fi
  IID=$(echo "$INST_JSON" | jq -r '.data.id')
fi

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
