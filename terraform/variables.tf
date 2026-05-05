variable "tenancy_ocid" {
  type        = string
  description = "OCID of your tenancy (Identity → Tenancy details)."
}

variable "user_ocid" {
  type        = string
  description = "OCID of the IAM user Terraform uses (Profile → User information)."
}

variable "api_fingerprint" {
  type        = string
  description = "API key fingerprint for that user (API keys list in OCI console)."
}

variable "private_key_path" {
  type        = string
  description = "Path to the PEM private key matching the uploaded public API key (~/.oci/oci_api_key.pem)."
}

variable "region" {
  type        = string
  description = "OCI region identifier, e.g. uk-london-1, us-ashburn-1."
}

variable "compartment_ocid" {
  type        = string
  description = "Compartment OCID where VCN and instance are created (often the root compartment)."
}

variable "project_name" {
  type        = string
  description = "Prefix for resource display names."
  default     = "mervyn"
}

variable "ssh_public_key" {
  type        = string
  description = "SSH public key for the default OS user (ubuntu on Canonical images)."
}

variable "ssh_allowed_cidrs" {
  type        = list(string)
  description = "CIDRs allowed to SSH (port 22). Use your home IP/32 for least exposure."
  default     = ["0.0.0.0/0"]
}

variable "expose_app_port" {
  type        = bool
  description = "If true, open TCP 3000 from the internet. Prefer false in production and use 80/443 + a reverse proxy."
  default     = false
}

variable "app_port_cidrs" {
  type        = list(string)
  description = "When expose_app_port is true, CIDRs allowed to reach the app port (default 3000)."
  default     = ["0.0.0.0/0"]
}

variable "http_cidrs" {
  type        = list(string)
  description = "CIDRs allowed on TCP 80/443 (Slack, Let's Encrypt, browsers)."
  default     = ["0.0.0.0/0"]
}

variable "instance_shape" {
  type        = string
  description = "OCI compute shape (e.g. VM.Standard.A1.Flex, VM.Standard.E2.1.Micro)."
  default     = "VM.Standard.A1.Flex"
}

variable "instance_ocpus" {
  type        = number
  description = "OCPUs for shape_config (E2.Micro: 1)."
  default     = 1
}

variable "instance_memory_gbs" {
  type        = number
  description = "Memory in GB for shape_config (A1.Flex default 6; E2.Micro: 1)."
  default     = 6
}

variable "instance_source_image_id" {
  type        = string
  description = "Optional boot image OCID; if empty, latest Ubuntu for ubuntu_version is selected."
  default     = ""
}

variable "instance_display_name" {
  type        = string
  description = "Instance display name; leave empty for mervyn-arm style default from project_name."
  default     = ""
}

variable "ssh_user" {
  type        = string
  description = "OS login for ssh output (ubuntu / opc)."
  default     = "ubuntu"
}

variable "availability_domain_index" {
  type        = number
  description = "AD index 0..n-1 (e.g. 1 for UK-LONDON-1-AD-2 when AD-1 is index 0)."
  default     = 0
}

variable "ubuntu_version" {
  type        = string
  description = "Ubuntu LTS version string as listed by OCI images (e.g. 22.04)."
  default     = "22.04"
}

variable "bootstrap_docker" {
  type        = bool
  description = "Cloud-init: install Docker Engine + Compose plugin (Ubuntu)."
  default     = true
}
