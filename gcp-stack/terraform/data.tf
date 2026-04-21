/**
 * Org billing and folder parent: read terraform-core bootstrap outputs (same contract as
 * terraform-framework-example-stack).
 */
data "terraform_remote_state" "terraform_core" {
  backend = "gcs"

  config = {
    bucket = var.terraform_core_remote_state_bucket
    prefix = var.terraform_core_remote_state_prefix
  }
}
