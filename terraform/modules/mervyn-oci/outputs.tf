output "instance_public_ip" {
  description = "Public IPv4 for SSH and (after TLS) Slack Events URL."
  value       = oci_core_instance.mervyn.public_ip
}

output "instance_ocid" {
  value = oci_core_instance.mervyn.id
}

output "vcn_id" {
  value = oci_core_vcn.this.id
}

output "ssh_command" {
  description = "SSH hint (ubuntu on Canonical; opc on Oracle Linux)."
  value       = "ssh ${var.ssh_user}@${oci_core_instance.mervyn.public_ip}"
}
