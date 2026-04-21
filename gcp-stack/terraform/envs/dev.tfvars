# Mervyn GCP — dev (project display name max 30 chars)
folder_name  = "playground-projects"
project_name = "mervyn-dev"

labels = {
  environment = "dev"
  stack       = "mervyn"
  managed-by  = "terraform"
}

# Optional: override root os_login default (see terraform/variables.tf).
