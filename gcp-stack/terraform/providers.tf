provider "google" {
  alias = "elevated"

  credentials                 = var.elevated_sa_credentials
  impersonate_service_account = var.elevated_impersonate_service_account
}

provider "google-beta" {
  alias = "elevated"

  credentials                 = var.elevated_sa_credentials
  impersonate_service_account = var.elevated_impersonate_service_account
}

locals {
  stack_provider_credentials = (
    var.stack_sa_credentials != null && var.stack_sa_credentials != "" ? var.stack_sa_credentials :
    var.elevated_sa_credentials != null && var.elevated_sa_credentials != "" ? var.elevated_sa_credentials :
    null
  )
}

provider "google" {
  alias = "stack"

  credentials                 = local.stack_provider_credentials
  impersonate_service_account = var.stack_sa_credentials != null ? null : var.elevated_impersonate_service_account
}

provider "google-beta" {
  alias = "stack"

  credentials                 = local.stack_provider_credentials
  impersonate_service_account = var.stack_sa_credentials != null ? null : var.elevated_impersonate_service_account
}
