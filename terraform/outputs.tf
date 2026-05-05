output "instance_public_ip" {
  description = "Public IPv4 for SSH and (after TLS) Slack Events URL."
  value       = module.oci_stack.instance_public_ip
}

output "instance_ocid" {
  value = module.oci_stack.instance_ocid
}

output "vcn_id" {
  value = module.oci_stack.vcn_id
}

output "ssh_command" {
  description = "ssh user comes from tfvars ssh_user (ubuntu or opc)."
  value       = module.oci_stack.ssh_command
}
