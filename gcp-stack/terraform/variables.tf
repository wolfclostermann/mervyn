variable "project_name" {
  description = "GCP project display name (google_project.name); max 30 characters."
  type        = string
}

variable "terraform_core_remote_state_bucket" {
  description = "GCS bucket holding terraform-core (bootstrap) remote state."
  type        = string
  default     = "gfs-terraform-bootstrap-tfstate"
}

variable "terraform_core_remote_state_prefix" {
  description = "State prefix for terraform-core in the bootstrap bucket."
  type        = string
  default     = "terraform-core"
}

variable "folder_name" {
  description = "Folder for this project (must match an entry in main.tf folder_ids)."
  type        = string
  default     = "playground-projects"

  validation {
    condition     = contains(["artifact-projects", "core-projects", "data-projects", "host-projects", "playground-projects", "service-projects", "SRE-projects"], var.folder_name)
    error_message = "folder_name must be one of the org folder keys (see main.tf locals.folder_ids)."
  }
}

variable "random_project_id" {
  type    = bool
  default = true
}

variable "default_service_account" {
  type    = string
  default = "disable"
}

variable "deletion_policy" {
  type    = string
  default = "PREVENT"
}

variable "activate_apis" {
  description = "Project APIs (OS Login + IAP used by gcp-compute-vm os_login; extend as needed)."
  type        = list(string)
  default = [
    "compute.googleapis.com",
    "iap.googleapis.com",
    "oslogin.googleapis.com",
  ]
}

variable "labels" {
  type    = map(string)
  default = {}
}

variable "group_name" {
  type    = string
  default = ""
}

variable "group_role" {
  type    = string
  default = "roles/editor"
}

variable "elevated_sa_credentials" {
  type      = string
  default   = null
  sensitive = true
}

variable "elevated_impersonate_service_account" {
  type     = string
  default  = null
  nullable = true
}

variable "stack_sa_credentials" {
  type      = string
  default   = null
  sensitive = true
}

variable "gcp_region" {
  type    = string
  default = "europe-west2"
}

variable "gcp_zone" {
  type    = string
  default = "europe-west2-a"
}

variable "vpc_network_name" {
  type    = string
  default = "mervyn-vpc"
}

variable "vpc_subnet_cidr" {
  type    = string
  default = "10.20.0.0/24"
}

variable "mervyn_vm_name" {
  type    = string
  default = "mervyn"
}

variable "mervyn_machine_type" {
  type    = string
  default = "e2-medium"
}

variable "mervyn_enable_external_ip" {
  type        = bool
  description = "Ephemeral public IP for simple internet egress (apt, Docker pulls, ngrok); not required for Slack (built-in ngrok). Disable and use Cloud NAT if you want a private instance."
  default     = true
}

variable "mervyn_boot_disk_image" {
  type    = string
  default = "ubuntu-os-cloud/ubuntu-2204-lts"
}

variable "mervyn_boot_disk_size_gb" {
  type    = number
  default = 30
}

variable "mervyn_boot_disk_type" {
  type    = string
  default = "pd-balanced"
}

variable "bootstrap_docker" {
  type    = bool
  default = true
}

variable "os_login" {
  description = <<-EOT
    Passed to terraform-framework gcp-compute-vm: instance OS Login metadata, optional project IAM
    (roles/compute.osLogin and osAdminLogin), and per-instance IAP TCP tunnel (iap.tunnelResourceAccessor).
    Set manage_iam=false to only enable OS Login on the VM and bind IAM outside this stack.
  EOT
  type = object({
    enabled            = optional(bool, true)
    require_two_factor = optional(bool, false)
    manage_iam         = optional(bool, true)
    ssh_users          = optional(list(string), [])
    ssh_groups         = optional(list(string), [])
    admin_users        = optional(list(string), [])
    admin_groups       = optional(list(string), [])
  })
  default = {
    enabled     = true
    manage_iam  = true
    admin_users = ["wolf.clostermann@gfsdeliver.com"]
  }
}
