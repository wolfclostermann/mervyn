/**
 * Mervyn on GCP — stack layout matches gfs-sre/terraform-framework-example-stack:
 * gcp-project + gcp-net-vpc + gcp-compute-vm from terraform-framework (Git),
 * org_id / billing_account from terraform-core remote state.
 *
 * Template parity: gfs-sre/terraform-framework-example-stack (same modules + remote state).
 */
locals {
  # Align folder placement with your org (same keys as terraform-framework-example-stack).
  folder_ids = {
    "artifact-projects"   = "893989721448"
    "core-projects"       = "12516760442"
    "data-projects"       = "117743516990"
    "host-projects"       = "143568231823"
    "playground-projects" = "299768287157"
    "service-projects"    = "523188137240"
    "SRE-projects"        = "758744073848"
  }
  folder_id = local.folder_ids[var.folder_name]

  cloud_init_path = abspath("${path.module}/../../terraform/cloud-init-docker.yaml")

  # Cloud-init only; OS Login metadata + IAM + IAP tunnel bindings come from gcp-compute-vm os_login (framework module).
  mervyn_metadata = var.bootstrap_docker ? {
    "user-data" = base64encode(file(local.cloud_init_path))
  } : {}
}

module "gcp_project" {
  source = "git::https://github.com/gfs-sre/terraform-framework.git//modules/gcp-project?ref=development"

  providers = {
    google      = google.elevated
    google-beta = google-beta.elevated
  }

  project_name    = var.project_name
  org_id          = data.terraform_remote_state.terraform_core.outputs.org_id
  billing_account = data.terraform_remote_state.terraform_core.outputs.billing_account
  folder_id       = local.folder_id

  random_project_id       = var.random_project_id
  default_service_account = var.default_service_account
  deletion_policy         = var.deletion_policy
  activate_apis           = var.activate_apis
  labels                  = var.labels
  group_name              = var.group_name
  group_role              = var.group_role
}

resource "google_service_account" "terraform_runner" {
  provider = google.elevated

  project      = module.gcp_project.project_id
  account_id   = "terraform-runner"
  display_name = "Terraform runner (project-scoped)"
}

resource "google_project_iam_member" "terraform_runner_editor" {
  provider = google.elevated

  project = module.gcp_project.project_id
  role    = "roles/editor"
  member  = "serviceAccount:${google_service_account.terraform_runner.email}"
}

module "gcp_net_vpc" {
  source = "git::https://github.com/gfs-sre/terraform-framework.git//modules/gcp-net-vpc?ref=development"

  providers = {
    google      = google.stack
    google-beta = google-beta.stack
  }

  project_id   = module.gcp_project.project_id
  network_name = var.vpc_network_name

  subnets = [
    {
      name                  = "primary"
      region                = var.gcp_region
      ip_cidr_range         = var.vpc_subnet_cidr
      private_google_access = true
    }
  ]

  iap_ssh_target_tags = ["ssh-iap"]
}

# Ingress: IAP TCP forwarding for SSH only (module gcp_net_vpc, tag ssh-iap). No public HTTP/app ports —
# Slack reaches Mervyn via built-in ngrok (outbound) or other paths you configure.

module "gcp_compute_vm" {
  source = "git::https://github.com/gfs-sre/terraform-framework.git//modules/gcp-compute-vm?ref=development"

  providers = {
    google      = google.stack
    google-beta = google-beta.stack
  }

  project_id           = module.gcp_project.project_id
  name                 = var.mervyn_vm_name
  zone                 = var.gcp_zone
  machine_type         = var.mervyn_machine_type
  subnetwork_self_link = module.gcp_net_vpc.subnet_self_links["primary"]
  network_tags         = ["ssh-iap"]
  labels               = var.labels
  enable_external_ip   = var.mervyn_enable_external_ip

  boot_disk = {
    image = var.mervyn_boot_disk_image
    size  = var.mervyn_boot_disk_size_gb
    type  = var.mervyn_boot_disk_type
  }

  metadata                = local.mervyn_metadata
  metadata_startup_script = null

  shielded_vm = {}

  os_login = var.os_login
}
