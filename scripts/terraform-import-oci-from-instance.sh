#!/usr/bin/env bash
# Given an existing OCI compute instance OCID (e.g. from the console URL), resolve the VCN objects
# this Terraform stack manages and print `terraform import` lines for module.oci_stack.* so you can
# adopt resources created by scripts/oci-bootstrap.sh (or anything matching the same layout).
#
# Prerequisites: oci + jq in PATH, API profile/region matching the instance (OCI_CLI_PROFILE, etc.).
#
# Layout expected (project prefix defaults to "mervyn"):
#   ${project}-vcn, ${project}-igw, ${project}-public-sl, ${project}-public subnet,
#   instance display name ${project}-arm
#
# Usage:
#   ./scripts/terraform-import-oci-from-instance.sh 'ocid1.instance.oc1....'
#   ./scripts/terraform-import-oci-from-instance.sh 'ocid1.instance.oc1....' myproj
#
# Then run the printed commands from the terraform/ directory after terraform init.
#
# If `terraform plan` wants to replace the instance (boot image / user_data differs from the
# ubuntu_arm data source), add a static lifecycle block inside resource oci_core_instance.mervyn
# in terraform/modules/mervyn-oci/main.tf:
#
#   lifecycle {
#     ignore_changes = [source_details, metadata, availability_domain]
#   }
#
set -euo pipefail

INSTANCE_OCID="${1:?usage: $0 <instance-ocid> [project-name]}"
PROJECT="${2:-mervyn}"

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
TF_ROOT="$ROOT/terraform"

for cmd in oci jq; do
  command -v "$cmd" >/dev/null 2>&1 || {
    echo "error: '$cmd' not found in PATH" >&2
    exit 1
  }
done

echo "==> Resolving network resources for instance ${INSTANCE_OCID}..." >&2

INST_JSON=$(oci compute instance get --instance-id "$INSTANCE_OCID" --output json)
COMPARTMENT=$(echo "$INST_JSON" | jq -r '.data."compartment-id" // .data.compartmentId // empty')
AD_NAME=$(echo "$INST_JSON" | jq -r '.data."availability-domain" // .data.availabilityDomain // empty')
INST_DN=$(echo "$INST_JSON" | jq -r '.data."display-name" // .data.displayName // empty')

[[ -n "$COMPARTMENT" && "$COMPARTMENT" != "null" ]] || {
  echo "error: could not read compartment-id from instance" >&2
  exit 1
}

echo "    display-name: $INST_DN (set instance_display_name in tfvars to match, or default is ${PROJECT}-arm)" >&2
echo "    availability-domain: $AD_NAME" >&2

VNIC_JSON=$(oci compute instance list-vnics --instance-id "$INSTANCE_OCID" --output json)
SUBNET_ID=$(echo "$VNIC_JSON" | jq -r '.data[0]."subnet-id" // .data[0].subnetId // empty')
[[ -n "$SUBNET_ID" && "$SUBNET_ID" != "null" ]] || {
  echo "error: could not read subnet-id from primary VNIC" >&2
  exit 1
}

SUBNET_JSON=$(oci network subnet get --subnet-id "$SUBNET_ID" --output json)
SUBNET_DN=$(echo "$SUBNET_JSON" | jq -r '.data."display-name" // .data.displayName // empty')
echo "    subnet: $SUBNET_DN ($SUBNET_ID) — Terraform expects ${PROJECT}-public" >&2

VCN_ID=$(echo "$SUBNET_JSON" | jq -r '.data."vcn-id" // .data.vcnId // empty')
VCN_DETAIL=$(oci network vcn get --vcn-id "$VCN_ID" --output json)
DEFAULT_RT=$(echo "$VCN_DETAIL" | jq -r '.data."default-route-table-id" // .data.defaultRouteTableId // empty')
VCN_DN=$(echo "$VCN_DETAIL" | jq -r '.data."display-name" // .data.displayName // empty')
echo "    vcn: $VCN_DN ($VCN_ID) — Terraform expects ${PROJECT}-vcn" >&2

SL_EXPECT="${PROJECT}-public-sl"
SL_ID=""
while read -r sl; do
  [[ -z "$sl" || "$sl" == "null" ]] && continue
  sl_json=$(oci network security-list get --security-list-id "$sl" --output json)
  dn=$(echo "$sl_json" | jq -r '.data."display-name" // .data.displayName // empty')
  if [[ "$dn" == "$SL_EXPECT" ]]; then
    SL_ID=$sl
    break
  fi
done < <(echo "$SUBNET_JSON" | jq -r '.data."security-list-ids"[]? // empty')

if [[ -z "$SL_ID" ]]; then
  echo "error: no security list named '${SL_EXPECT}' attached to this subnet." >&2
  echo "       Attached lists:" >&2
  while read -r sl; do
    [[ -z "$sl" ]] && continue
    oci network security-list get --security-list-id "$sl" --query 'data."display-name"' --raw-output 2>/dev/null | sed 's/^/         /' >&2 || true
  done < <(echo "$SUBNET_JSON" | jq -r '.data."security-list-ids"[]? // empty')
  exit 1
fi

IGW_JSON=$(oci network internet-gateway list --compartment-id "$COMPARTMENT" --vcn-id "$VCN_ID" --output json)
IGW_EXPECT="${PROJECT}-igw"
IGW_ID=$(echo "$IGW_JSON" | jq -r --arg dn "$IGW_EXPECT" '.data[] | select((."display-name" // .displayName // "") == $dn) | .id' | head -1)

if [[ -z "$IGW_ID" ]]; then
  echo "error: no internet gateway named '${IGW_EXPECT}' on VCN ${VCN_ID}" >&2
  exit 1
fi

[[ -n "$DEFAULT_RT" && "$DEFAULT_RT" != "null" ]] || {
  echo "error: could not read default-route-table-id from VCN" >&2
  exit 1
}

echo "" >&2
echo "Run these from ${TF_ROOT} (after terraform init). Order matters for clarity; Terraform accepts any order." >&2
echo "" >&2

cat <<EOF
cd '${TF_ROOT}'
terraform import 'module.oci_stack.oci_core_vcn.this' '${VCN_ID}'
terraform import 'module.oci_stack.oci_core_internet_gateway.this' '${IGW_ID}'
terraform import 'module.oci_stack.oci_core_default_route_table.public' '${DEFAULT_RT}'
terraform import 'module.oci_stack.oci_core_security_list.public' '${SL_ID}'
terraform import 'module.oci_stack.oci_core_subnet.public' '${SUBNET_ID}'
terraform import 'module.oci_stack.oci_core_instance.mervyn' '${INSTANCE_OCID}'
EOF

echo ""
echo "# Next: ensure terraform.tfvars matches this compartment/region/SSH key and shape settings."
echo "# If plan tries to replace the instance, see lifecycle comment at top of: scripts/terraform-import-oci-from-instance.sh"
echo "# Align availability_domain_index with AD '${AD_NAME}' (0-based index from: oci iam availability-domain list --compartment-id <tenancy-ocid>)."
