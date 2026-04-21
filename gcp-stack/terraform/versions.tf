terraform {
  required_version = ">= 1.14"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = ">= 5.41"
    }
    google-beta = {
      source  = "hashicorp/google-beta"
      version = ">= 5.41"
    }
  }
}
