output "project_id" {
  description = "Created GCP project ID."
  value       = module.gcp_project.project_id
}

output "project_number" {
  description = "GCP project number."
  value       = module.gcp_project.project_number
}

output "terraform_runner_sa_email" {
  description = "Project Terraform runner SA (CI / WIF)."
  value       = google_service_account.terraform_runner.email
}

output "mervyn_vm_self_link" {
  description = "Compute Engine instance self link."
  value       = module.gcp_compute_vm.self_link
}

output "mervyn_internal_ip" {
  value = module.gcp_compute_vm.internal_ip
}

output "mervyn_external_ip" {
  description = "Public IP when mervyn_enable_external_ip is true (egress / convenience; Slack uses in-app ngrok)."
  value       = module.gcp_compute_vm.external_ip
}

output "ssh_via_iap_example" {
  description = "IAP TCP tunnel + OS Login. With gcloud default project and compute/zone matching this stack, this command is enough; otherwise add --project and --zone (see project_id and gcp_zone in your tfvars / plan output)."
  value       = "gcloud compute ssh ${var.mervyn_vm_name} --tunnel-through-iap"
}

output "vpc_network_self_link" {
  value = module.gcp_net_vpc.network_self_link
}

output "vpc_subnet_self_links" {
  value = module.gcp_net_vpc.subnet_self_links
}
