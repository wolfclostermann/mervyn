#!/usr/bin/env bash
# Choose how to provision a Mervyn host.
#
# Usage:
#   ./scripts/bootstrap.sh                       # OCI CLI bootstrap (default)
#   ./scripts/deploy-oci.sh ubuntu@<public-ip>   # push app over SSH after bootstrap
#   MERVYN_CLOUD_PROVIDER=aws ./scripts/bootstrap.sh
#   ./scripts/bootstrap.sh oci [oci-bootstrap args...]
#   ./scripts/bootstrap.sh aws                  # print Terraform AWS instructions
#
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

if [[ $# -ge 1 && "$1" =~ ^(oci|aws)$ ]]; then
  provider=$(echo "$1" | tr '[:upper:]' '[:lower:]')
  shift
else
  provider=$(echo "${MERVYN_CLOUD_PROVIDER:-oci}" | tr '[:upper:]' '[:lower:]')
fi

case "$provider" in
oci)
  exec "$SCRIPT_DIR/oci-bootstrap.sh" "$@"
  ;;
aws)
  cat <<EOF
AWS: use Terraform (separate root; only the AWS provider — no OCI credentials needed).

  cd "$REPO_ROOT/terraform/stacks/aws"
  cp terraform.tfvars.example terraform.tfvars   # edit ssh_public_key, aws_region, etc.
  terraform init
  terraform apply

Shared cloud-init: $REPO_ROOT/terraform/cloud-init-docker.yaml
EOF
  ;;
*)
  echo "error: unknown cloud provider '$provider' (use: oci, aws)" >&2
  exit 1
  ;;
esac
